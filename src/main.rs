use std::collections::HashSet;
use std::convert::TryFrom;
use std::iter::FromIterator;

use anyhow::Context;
use dialoguer::Confirmation;
use git2::{BranchType, Repository};
use log::*;

use git_trim::args::Args;
use git_trim::config::{self, Config, ConfigValue};
use git_trim::{
    delete_local_branches, delete_remote_branches, get_trim_plan, remote_update, ClassifiedBranch,
    Git, LocalBranch, PlanParam, RemoteBranchError, RemoteTrackingBranch, TrimPlan,
};

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

    if git.repo.remotes()?.is_empty() {
        return Err(anyhow::anyhow!("git-trim requires at least one remote").into());
    }

    let config = Config::read(&git.repo, &git.config, &args)?;
    info!("config: {:?}", config);
    if config.bases.is_empty() {
        return error_no_bases();
    }

    if *config.update {
        if should_update(
            &git,
            *config.update_interval,
            matches!(config.update, ConfigValue::Explicit { value: true , .. }),
        )? {
            remote_update(&git.repo, args.dry_run)?;
            println!();
        } else {
            println!("Repository is updated recently. Skip to update it")
        }
    }

    let plan = get_trim_plan(
        &git,
        &PlanParam {
            bases: config.bases.iter().map(String::as_str).collect(),
            protected_branches: config.protected.iter().map(String::as_str).collect(),
            filter: config.filter.clone(),
            detach: *config.detach,
        },
    )?;

    print_summary(&plan, &git.repo)?;

    let locals = plan.locals_to_delete();
    let remotes = plan.remotes_to_delete();
    let any_branches_to_remove = !(locals.is_empty() && remotes.is_empty());

    if !args.dry_run
        && *config.confirm
        && any_branches_to_remove
        && !Confirmation::new()
            .with_text("Confirm?")
            .default(false)
            .interact()?
    {
        println!("Cancelled");
        return Ok(());
    }

    delete_remote_branches(&git.repo, &remotes, args.dry_run)?;
    delete_local_branches(&git.repo, &locals, args.dry_run)?;
    Ok(())
}

fn error_no_bases() -> Result<()> {
    eprintln!(
        "\
No base branch is found! Try following any resolution:
 * `git remote set-head origin --auto` will sync `refs/remotes/origin/HEAD` automatically.
 * `git config trim.bases develop,master` will set base branches for git-trim for a repository.
 * `git config --global trim.bases develop,master` will set base branches for `git-trim` globally.
 * `git trim --bases develop,master` will temporarily set base branches for `git-trim`"
    );

    Err(anyhow::anyhow!("No base branch is found!").into())
}

pub fn print_summary(plan: &TrimPlan, repo: &Repository) -> Result<()> {
    println!("Branches that will remain:");
    println!("  local branches:");
    let local_branches_to_delete = HashSet::<_>::from_iter(plan.locals_to_delete());
    for local_branch in repo.branches(Some(BranchType::Local))? {
        let (branch, _) = local_branch?;
        let branch_name = branch.name()?.context("non utf-8 local branch name")?;
        let refname = branch.get().name().context("non utf-8 local refname")?;
        let branch = LocalBranch::new(refname);
        if local_branches_to_delete.contains(&branch) {
            continue;
        }
        if let Some(preserved) = plan.get_preserved_local(&branch) {
            println!(
                "    {} [{}, but: {}]",
                branch_name,
                preserved.branch.class(),
                preserved.reason
            );
        } else {
            println!("    {}", branch_name);
        }
    }
    println!("  remote references:");
    let remote_refs_to_delete = HashSet::<_>::from_iter(plan.remotes_to_delete());
    let mut printed_remotes = HashSet::new();
    for remote_ref in repo.branches(Some(BranchType::Remote))? {
        let (branch, _) = remote_ref?;
        let refname = branch.get().name().context("non utf-8 remote ref name")?;
        let shorthand = branch
            .get()
            .shorthand()
            .context("non utf-8 remote ref name")?;
        let remote_branch = match RemoteTrackingBranch::new(&refname).to_remote_branch(&repo) {
            Ok(remote_branch) => remote_branch,
            Err(RemoteBranchError::RemoteNotFound) => continue,
            Err(err) => return Err(err.into()),
        };
        if remote_refs_to_delete.contains(&remote_branch) {
            continue;
        }
        if let Some(preserved) = plan.get_preserved_remote(&remote_branch) {
            println!(
                "    {} [{}, but: {}]",
                shorthand,
                preserved.branch.class(),
                preserved.reason
            );
        } else {
            println!("    {}", shorthand);
        }
        printed_remotes.insert(remote_branch);
    }
    for preserved in &plan.preserved {
        match &preserved.branch {
            ClassifiedBranch::MergedRemote(remote) | ClassifiedBranch::StrayRemote(remote) => {
                if !printed_remotes.contains(&remote) {
                    println!(
                        "    {} [{}, but: {}]",
                        remote.to_string(),
                        preserved.branch.class(),
                        preserved.reason,
                    );
                }
            }
            _ => {}
        }
    }
    println!();

    let mut merged_locals = Vec::new();
    let mut merged_remotes = Vec::new();
    let mut stray_locals = Vec::new();
    let mut stray_remotes = Vec::new();
    for branch in &plan.to_delete {
        match branch {
            ClassifiedBranch::MergedLocal(local) => {
                merged_locals.push(local.short_name().to_owned())
            }
            ClassifiedBranch::StrayLocal(local) => stray_locals.push(local.short_name().to_owned()),
            ClassifiedBranch::MergedRemote(remote) => merged_remotes.push(remote.to_string()),
            ClassifiedBranch::StrayRemote(remote) => stray_remotes.push(remote.to_string()),
        }
    }

    fn print(label: &str, mut branches: Vec<String>) -> Result<()> {
        if branches.is_empty() {
            return Ok(());
        }
        branches.sort();
        println!("Delete {}:", label);
        for branch in branches {
            println!("  - {}", branch);
        }
        Ok(())
    }

    print("merged local branches", merged_locals)?;
    print("merged remote refs", merged_remotes)?;
    print("stray local branches", stray_locals)?;
    print("stray remote refs", stray_remotes)?;

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
        .with_default(false)
        .read()?
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
