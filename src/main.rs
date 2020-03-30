use std::collections::HashSet;
use std::convert::TryFrom;
use std::iter::FromIterator;

use anyhow::Context;
use dialoguer::Confirmation;
use git2::{BranchType, Repository};
use log::*;

use git_trim::args::{Args, DeleteFilter};
use git_trim::config::{CommaSeparatedSet, ConfigValue};
use git_trim::{
    config, Config, Git, MergedOrStrayAndKeptBacks, RemoteBranchError, RemoteTrackingBranch,
};
use git_trim::{delete_local_branches, delete_remote_branches, get_merged_or_stray, remote_update};

type Result<T> = ::std::result::Result<T, Error>;
type Error = Box<dyn std::error::Error>;

#[paw::main]
fn main(args: Args) -> Result<()> {
    env_logger::init();
    info!("SEMVER: {}", env!("VERGEN_SEMVER"));
    info!("SHA: {}", env!("VERGEN_SHA"));
    info!("COMMIT_DATE: {}", env!("VERGEN_COMMIT_DATE"));
    info!("TARGET_TRIPLE: {}", env!("VERGEN_TARGET_TRIPLE"));

    let git = Git::try_from(Repository::open_from_env()?)?;

    let bases = config::get(&git.config, "trim.bases")
        .with_explicit("cli", non_empty(args.bases.clone()))
        .with_default(&CommaSeparatedSet::from_iter(vec![
            String::from("develop"),
            String::from("master"),
        ]))
        .read_multi()?;
    let protected = config::get(&git.config, "trim.protected")
        .with_explicit("cli", non_empty(args.protected.clone()))
        .with_default(&CommaSeparatedSet::from_iter(bases.iter().cloned()))
        .read_multi()?;
    let update = config::get(&git.config, "trim.update")
        .with_explicit("cli", args.update())
        .with_default(&true)
        .read()?
        .expect("has default");
    let update_interval = config::get(&git.config, "trim.updateInterval")
        .with_explicit("cli", args.update_interval)
        .with_default(&5)
        .parse()?
        .expect("has default in structopt");
    let confirm = config::get(&git.config, "trim.confirm")
        .with_explicit("cli", args.confirm())
        .with_default(&true)
        .read()?
        .expect("has default");
    let detach = config::get(&git.config, "trim.detach")
        .with_explicit("cli", args.detach())
        .with_default(&true)
        .read()?
        .expect("has default");
    let filter = config::get(&git.config, "trim.delete")
        .with_explicit(
            "cli",
            non_empty(args.delete.clone()).map(|x| x.into_iter().flatten().collect()),
        )
        .with_default(&DeleteFilter::merged_origin())
        .parse_flatten()?
        .expect("has default");

    info!("bases: {:?}", bases);
    info!("protected: {:?}", protected);
    info!("update: {:?}", update);
    info!("confirm: {:?}", confirm);
    info!("detach: {:?}", detach);
    info!("filter: {:?}", filter);

    if git.repo.remotes()?.is_empty() {
        return Err(anyhow::anyhow!("git-trim requires at least one remote").into());
    }

    if *update {
        if should_update(
            &git,
            *update_interval,
            matches!(update, ConfigValue::Explicit { value: true , .. }),
        )? {
            remote_update(&git.repo, args.dry_run)?;
            println!();
        } else {
            println!("Repository is updated recently. Skip to update it")
        }
    }

    let branches = get_merged_or_stray(
        &git,
        &Config {
            bases: bases.iter().map(String::as_str).collect(),
            protected_branches: protected.iter().map(String::as_str).collect(),
            filter: filter.clone(),
            detach: *detach,
        },
    )?;

    print_summary(&branches, &git.repo)?;

    let to_delete = branches.to_delete;
    let any_branches_to_remove = !(to_delete.locals().is_empty() && to_delete.remotes().is_empty());

    if !args.dry_run
        && *confirm
        && any_branches_to_remove
        && !Confirmation::new()
            .with_text("Confirm?")
            .default(false)
            .interact()?
    {
        println!("Cancelled");
        return Ok(());
    }

    delete_remote_branches(&git.repo, &to_delete.remotes(), args.dry_run)?;
    delete_local_branches(&git.repo, &to_delete.locals(), args.dry_run)?;
    Ok(())
}

fn non_empty<T>(x: Vec<T>) -> Option<Vec<T>> {
    if x.is_empty() {
        None
    } else {
        Some(x)
    }
}

pub fn print_summary(branches: &MergedOrStrayAndKeptBacks, repo: &Repository) -> Result<()> {
    println!("Branches that will remain:");
    println!("  local branches:");
    let local_branches_to_delete: HashSet<_> = branches.to_delete.locals().into_iter().collect();
    for local_branch in repo.branches(Some(BranchType::Local))? {
        let (branch, _) = local_branch?;
        let name = branch.name()?.context("non utf-8 local branch name")?;
        if local_branches_to_delete.contains(name) {
            continue;
        }
        if let Some(reason) = branches.kept_backs.get(name) {
            println!(
                "    {} [{}, but: {}]",
                name, reason.original_classification, reason.message
            );
        } else {
            println!("    {}", name);
        }
    }
    println!("  remote references:");
    let remote_refs_to_delete: HashSet<_> = branches.to_delete.remotes().into_iter().collect();
    let mut printed_remotes = HashSet::new();
    for remote_ref in repo.branches(Some(BranchType::Remote))? {
        let (branch, _) = remote_ref?;
        let name = branch.get().name().context("non utf-8 remote ref name")?;
        let remote_branch = match RemoteTrackingBranch::new(&name).remote_branch(&repo) {
            Ok(remote_branch) => remote_branch,
            Err(RemoteBranchError::RemoteNotFound) => continue,
            Err(err) => return Err(err.into()),
        };
        if remote_refs_to_delete.contains(&remote_branch) {
            continue;
        }
        if let Some(reason) = branches.kept_back_remotes.get(&remote_branch) {
            println!(
                "    {} [{}, but: {}]",
                name, reason.original_classification, reason.message
            );
        } else {
            println!("    {}", name);
        }
        printed_remotes.insert(remote_branch);
    }
    for (remote_branch, reason) in branches.kept_back_remotes.iter() {
        if !printed_remotes.contains(&remote_branch) {
            println!(
                "    {} [{}, but: {}]",
                remote_branch.to_string(),
                reason.original_classification,
                reason.message
            );
        }
    }
    println!();

    fn print<T>(label: &str, branches: &HashSet<T>)
    where
        T: std::fmt::Display + std::cmp::Ord,
    {
        if branches.is_empty() {
            return;
        }
        let mut branches: Vec<_> = branches.iter().collect();
        branches.sort();
        println!("Delete {}:", label);
        for branch in branches {
            println!("  - {}", branch);
        }
    }

    print("merged local branches", &branches.to_delete.merged_locals);
    print("merged remote refs", &branches.to_delete.merged_remotes);
    print("stray local branches", &branches.to_delete.stray_locals);
    print("stray remote refs", &branches.to_delete.stray_remotes);

    Ok(())
}

fn should_update(git: &Git, interval: u64, explicit: bool) -> Result<bool> {
    if interval == 0 {
        return Ok(true);
    }

    if explicit {
        trace!("explicitly set --update. force update");
        return Ok(true);
    }

    let auto_prune = config::get(&git.config, "fetch.prune")
        .with_default(&false)
        .parse()?
        .expect("default is provided");
    if !*auto_prune {
        trace!("`git config fetch.prune` is false. force update");
        return Ok(true);
    }

    let fetch_head = git.repo.path().join("FETCH_HEAD");
    if !fetch_head.exists() {
        return Ok(true);
    }

    let metadata = std::fs::metadata(fetch_head)?;
    let elapsed = match metadata.modified()?.elapsed() {
        Ok(elapsed) => elapsed,
        Err(_) => return Ok(true),
    };

    Ok(elapsed.as_secs() >= interval)
}
