mod remote_head_change_checker;

use std::collections::HashSet;
use std::convert::TryFrom;
use std::iter::FromIterator;

use anyhow::{Context, Result};
use clap::Parser;
use dialoguer::Confirm;
use git2::{BranchType, Repository};
use log::*;

use git_trim::args::Args;
use git_trim::config::{self, get, Config, ConfigValue};
use git_trim::{
    delete_local_branches, delete_remote_branches, get_trim_plan, ls_remote_head, remote_update,
    ClassifiedBranch, ForceSendSync, Git, LocalBranch, PlanParam, RemoteHead, RemoteTrackingBranch,
    SkipSuggestion, TrimPlan,
};

fn main() -> Result<()> {
    let args = Args::parse();

    env_logger::init();
    info!("SEMVER: {}", env!("VERGEN_BUILD_SEMVER"));
    info!("SHA: {:?}", option_env!("VERGEN_GIT_SHA"));
    info!(
        "COMMIT_DATE: {:?}",
        option_env!("VERGEN_GIT_COMMIT_TIMESTAMP")
    );
    info!("TARGET_TRIPLE: {}", env!("VERGEN_CARGO_TARGET_TRIPLE"));

    let git = Git::try_from(Repository::open_from_env()?)?;

    if git.repo.remotes()?.is_empty() {
        return Err(anyhow::anyhow!("git-trim requires at least one remote"));
    }

    let config = Config::read(&git.repo, &git.config, &args)?;
    info!("config: {:?}", config);
    if config.bases.is_empty() {
        return error_no_bases(&git.repo, &config.bases);
    }

    let mut checker = None;
    if *config.update {
        if should_update(&git, *config.update_interval, config.update)? {
            checker = Some(remote_head_change_checker::RemoteHeadChangeChecker::spawn()?);
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
            protected_patterns: config.protected.iter().map(String::as_str).collect(),
            delete: config.delete.clone(),
            detach: *config.detach,
        },
    )?;

    print_summary(&plan, &git.repo)?;

    let locals = plan.locals_to_delete();
    let remotes = plan.remotes_to_delete(&git.repo)?;
    let any_branches_to_remove = !(locals.is_empty() && remotes.is_empty());

    if !args.dry_run
        && *config.confirm
        && any_branches_to_remove
        && !Confirm::new()
            .with_prompt("Confirm?")
            .default(false)
            .interact()?
    {
        println!("Cancelled");
        return Ok(());
    }

    delete_remote_branches(&git.repo, remotes.as_slice(), args.dry_run)?;
    delete_local_branches(&git.repo, &locals, args.dry_run)?;

    prompt_survey_on_push_upstream(&git)?;

    if let Some(checker) = checker.take() {
        checker.check_and_notify(&git.repo)?;
    }
    Ok(())
}

fn error_no_bases(repo: &Repository, bases: &ConfigValue<HashSet<String>>) -> Result<()> {
    fn eprint_bullet(s: &str) {
        let width = textwrap::termwidth().max(40) - 4;
        for (i, line) in textwrap::wrap(s, width).iter().enumerate() {
            if i == 0 {
                eprintln!(" * {}", line);
            } else {
                eprintln!("   {}", line);
            }
        }
    }
    const GENERAL_HELP: &[&str] = &[
        "`git config trim.bases develop,main` for a repository.",
        "`git config --global trim.bases develop,main` to set globally.",
        "`git trim --bases develop,main` to set temporarily.",
    ];
    match bases {
        ConfigValue::Explicit(_) => {
            eprintln!(
                "I found that you passed an empty value to the CLI option `--bases`. Don't do that."
            );
        }
        ConfigValue::GitConfig(_) => {
            eprintln!(
                "I found that `git config trim.bases` is empty! Try any following commands to set valid bases:"
            );
            for help in GENERAL_HELP {
                eprint_bullet(help);
            }
        }
        ConfigValue::Implicit(_) => {
            let remotes = repo.remotes()?;
            let remotes: Vec<_> = remotes.iter().collect();
            if remotes.len() == 1 {
                let remote = remotes[0].expect("non utf-8 remote name");
                eprintln!("I can't detect base branch! Try following any resolution:");
                eprint_bullet(&format!(
                    "\
`git remote set-head {remote} --auto` will help `git-trim` to automatically detect the base branch.
If you see `{remote}/HEAD set to <base branch>` in the output of the previous command, \
then `git branch --set-upstream {remote}/<base branch> <base branch>` to set an upstream branch for <base branch> if exists.",
                    remote = remote
                ));
            } else {
                eprintln!("I can't detect base branch! Try following any resolution:");
                eprint_bullet(
                    "\
`git remote set-head <remote> --auto` will help `git-trim` to automatically detect the base branch.
Following command will sync all remotes for you:
`for REMOTE in $(git remote); do git remote set-head \"$REMOTE\" --auto; done`
Pick an appropriate one in mind if you see multiple `<remote>/HEAD set to <base branch>` in the output of the previous command.
Then `git branch --set-upstream <remote>/<base branch> <base branch>` to set an upstream branch for <base branch> if exists.",
                );
            }
            println!("You also can set bases manually with any of following commands:");
            for help in GENERAL_HELP {
                eprint_bullet(help);
            }
        }
    }

    Err(anyhow::anyhow!("No base branch is found!"))
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
            if preserved.base && matches!(preserved.branch, ClassifiedBranch::MergedLocal(_)) {
                println!("    {} [{}]", branch_name, preserved.reason);
            } else {
                println!(
                    "    {} [{}, but: {}]",
                    branch_name,
                    preserved.branch.message_local(),
                    preserved.reason
                );
            }
        } else if let Some(suggestion) = plan.skipped.get(refname) {
            println!("    {} *{}", branch_name, suggestion.kind());
        } else {
            println!("    {}", branch_name);
        }
    }
    println!("  remote references:");
    let remote_refs_to_delete = HashSet::<_>::from_iter(plan.remotes_to_delete(repo)?);
    let mut printed_remotes = HashSet::new();
    for remote_ref in repo.branches(Some(BranchType::Remote))? {
        let (branch, _) = remote_ref?;
        if branch.get().symbolic_target_bytes().is_some() {
            continue;
        }
        let refname = branch.get().name().context("non utf-8 remote ref name")?;
        let shorthand = branch
            .get()
            .shorthand()
            .context("non utf-8 remote ref name")?;
        let upstream = RemoteTrackingBranch::new(refname);
        let remote_branch = upstream.to_remote_branch(repo)?;
        if remote_refs_to_delete.contains(&remote_branch) {
            continue;
        }
        if let Some(preserved) = plan.get_preserved_upstream(&upstream) {
            if preserved.base
                && matches!(preserved.branch, ClassifiedBranch::MergedRemoteTracking(_))
            {
                println!("    {} [{}]", shorthand, preserved.reason);
            } else {
                println!(
                    "    {} [{}, but: {}]",
                    shorthand,
                    preserved.branch.message_remote(),
                    preserved.reason
                );
            }
        } else if let Some(suggestion) = plan.skipped.get(refname) {
            println!("    {} *{}", shorthand, suggestion.kind());
        } else {
            println!("    {}", shorthand);
        }
        printed_remotes.insert(remote_branch);
    }
    for preserved in &plan.preserved {
        match &preserved.branch {
            ClassifiedBranch::MergedDirectFetch { remote, .. }
            | ClassifiedBranch::DivergedDirectFetch { remote, .. } => {
                println!(
                    "    {} [{}, but: {}]",
                    remote,
                    preserved.branch.message_remote(),
                    preserved.reason,
                );
            }
            _ => {}
        }
    }

    if !plan.skipped.is_empty() {
        println!("  Some branches are skipped. Consider following to scan them:");
        let tracking = plan
            .skipped
            .values()
            .any(|suggest| suggest == &SkipSuggestion::Tracking);
        let tracking_remotes: Vec<_> = {
            let mut tmp = Vec::new();
            for suggest in plan.skipped.values() {
                if let SkipSuggestion::TrackingRemote(r) = suggest {
                    tmp.push(r);
                }
            }
            tmp
        };
        if let [single] = tracking_remotes.as_slice() {
            println!(
                "    *{}: Add `--delete 'merged:{}'` flag.",
                SkipSuggestion::KIND_TRACKING,
                single
            );
        } else if tracking_remotes.len() > 1 {
            println!(
                "    *{}: Add `--delete 'merged:*'` flag.",
                SkipSuggestion::KIND_TRACKING,
            );
        } else if tracking {
            println!(
                "    *{}: Add `--delete 'merged-local'` flag.",
                SkipSuggestion::KIND_TRACKING,
            );
        }
        let non_tracking = plan
            .skipped
            .values()
            .any(|suggest| suggest == &SkipSuggestion::NonTracking);
        if non_tracking {
            println!(
                "    *{}: Set an upstream to make it a tracking branch or add `--delete 'local'` flag.",
                SkipSuggestion::KIND_NON_TRACKING,
            );
        }

        let non_upstream_remotes: Vec<_> = {
            let mut tmp = Vec::new();
            for suggest in plan.skipped.values() {
                if let SkipSuggestion::NonUpstream(r) = suggest {
                    tmp.push(r);
                }
            }
            tmp
        };
        if let [single] = non_upstream_remotes.as_slice() {
            println!(
                "    *{}: Make it upstream of a tracking branch or add `--delete 'remote:{}'` flag.",
                SkipSuggestion::KIND_NON_UPSTREAM,
                single
            );
        } else if non_upstream_remotes.len() > 1 {
            println!(
                "    *{}: Make it upstream of a tracking branch or add `--delete 'remote:*'` flag.",
                SkipSuggestion::KIND_NON_UPSTREAM,
            );
        }
    }
    println!();

    let mut merged_locals = Vec::new();
    let mut merged_remotes = Vec::new();
    let mut stray = Vec::new();
    let mut diverged_remotes = Vec::new();
    for branch in &plan.to_delete {
        match branch {
            ClassifiedBranch::MergedLocal(local) => {
                merged_locals.push(local.short_name().to_owned())
            }
            ClassifiedBranch::Stray(local) => stray.push(local.short_name().to_owned()),
            ClassifiedBranch::MergedRemoteTracking(upstream) => {
                let remote = upstream.to_remote_branch(repo)?;
                merged_remotes.push(remote.to_string())
            }
            ClassifiedBranch::DivergedRemoteTracking { local, upstream } => {
                let remote = upstream.to_remote_branch(repo)?;
                merged_locals.push(local.short_name().to_owned());
                diverged_remotes.push(remote.to_string())
            }
            ClassifiedBranch::MergedDirectFetch { local, remote }
            | ClassifiedBranch::DivergedDirectFetch { local, remote } => {
                merged_locals.push(local.short_name().to_owned());
                diverged_remotes.push(remote.to_string())
            }
            ClassifiedBranch::MergedNonTrackingLocal(local) => {
                merged_locals.push(format!("{} (non-tracking)", local.short_name()));
            }
            ClassifiedBranch::MergedNonUpstreamRemoteTracking(upstream) => {
                let remote = upstream.to_remote_branch(repo)?;
                merged_remotes.push(format!("{} (non-upstream)", remote));
            }
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
    print("stray local branches", stray)?;
    print("diverged remote refs", diverged_remotes)?;

    Ok(())
}

fn should_update(git: &Git, interval: u64, config_update: ConfigValue<bool>) -> Result<bool> {
    if interval == 0 {
        return Ok(true);
    }

    if matches!(config_update, ConfigValue::Explicit(true)) {
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

fn prompt_survey_on_push_upstream(git: &Git) -> Result<()> {
    for remote_name in git.repo.remotes()?.iter() {
        let remote_name = remote_name.context("non-utf8 remote name")?;
        let key = format!("remote.{}.push", remote_name);
        if get::<String>(&git.config, &key).read()?.is_some() {
            println!(
                r#"

Help wanted!
I recognize that you've set a config `git config remote.{}.push`!
I once (mis)used that config to classify branches, but I retracted it after realizing that I don't understand the config well.
It would be very helpful to me if you share your use cases of the config to me.
Here's the survey URL: https://github.com/foriequal0/git-trim/issues/134
Thank you!
                "#,
                remote_name
            );
            break;
        }
    }
    Ok(())
}
