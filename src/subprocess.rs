use std::collections::HashSet;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use git2::{BranchType, Config, Reference, Repository};
use log::*;

use crate::branch::{get_fetch_upstream, RemoteTrackingBranch};
use crate::config::get_remote;

fn git(repo: &Repository, args: &[&str], level: log::Level) -> Result<()> {
    let workdir = repo.workdir().context("Bare repository is not supported")?;
    let workdir = workdir.to_str().context("non utf-8 workdir")?;
    log!(level, "> git -C {} {}", workdir, args.join(" "));

    let mut cd_args = vec!["-C", workdir];
    cd_args.extend_from_slice(args);
    let exit_status = Command::new("git").args(cd_args).status()?;
    if !exit_status.success() {
        Err(std::io::Error::from_raw_os_error(exit_status.code().unwrap_or(-1)).into())
    } else {
        Ok(())
    }
}

fn git_output(repo: &Repository, args: &[&str], level: log::Level) -> Result<String> {
    let workdir = repo.workdir().context("Bare repository is not supported")?;
    let workdir = workdir.to_str().context("non utf-8 workdir")?;
    log!(level, "> git -C {} {}", workdir, args.join(" "));

    let mut cd_args = vec!["-C", workdir];
    cd_args.extend_from_slice(args);
    let output = Command::new("git")
        .args(cd_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::from_raw_os_error(output.status.code().unwrap_or(-1)).into());
    }

    let str = std::str::from_utf8(&output.stdout)?.trim();
    for line in str.lines() {
        trace!("{}", line);
    }
    Ok(str.to_string())
}

pub fn remote_update(repo: &Repository, dry_run: bool) -> Result<()> {
    if !dry_run {
        git(repo, &["remote", "update", "--prune"], Level::Info)
    } else {
        info!("> git remote update --prune (dry-run)");
        Ok(())
    }
}

pub fn is_merged_by_rev_list(repo: &Repository, base: &str, commit: &str) -> Result<bool> {
    let range = format!("{}...{}", base, commit);
    // Is there any revs that are not applied to the base in the branch?
    let output = git_output(
        repo,
        &[
            "rev-list",
            "--cherry-pick",
            "--right-only",
            "--no-merges",
            "-n1",
            &range,
        ],
        Level::Trace,
    )?;

    // empty output means there aren't any revs that are not applied to the base.
    Ok(output.is_empty())
}

pub fn get_noff_merged_locals(
    repo: &Repository,
    config: &Config,
    bases: &[RemoteTrackingBranch],
) -> Result<HashSet<String>> {
    let mut result = HashSet::new();
    for base in bases {
        let branch_names = git_output(
            repo,
            &[
                "branch",
                "--format",
                "%(refname:short)",
                "--merged",
                &base.refname,
            ],
            Level::Trace,
        )?;
        for branch_name in branch_names.lines() {
            debug!("refname: {}", branch_name);
            if get_remote(config, branch_name)?.is_implicit() {
                debug!("skip: it is not a tracking branch");
                continue;
            }
            let upstream = get_fetch_upstream(repo, config, branch_name)?;
            if Some(base) == upstream.as_ref() {
                debug!("skip: {} tracks {:?}", branch_name, base);
                continue;
            }
            let branch = repo.find_branch(&branch_name, BranchType::Local)?;
            if branch.get().symbolic_target().is_some() {
                debug!("skip: it is symbolic");
                continue;
            }
            let branch_name = branch.name()?.context("no utf-8 branch name")?.to_string();
            debug!("noff merged local: it is merged to {:?}", base);
            result.insert(branch_name);
        }
    }
    Ok(result)
}

pub fn ls_remote_heads(repo: &Repository, remote_name: &str) -> Result<HashSet<String>> {
    let mut result = HashSet::new();
    for line in git_output(repo, &["ls-remote", "--heads", remote_name], Level::Trace)?.lines() {
        let records = line.split_whitespace().collect::<Vec<_>>();
        result.insert(records[1].to_string());
    }
    Ok(result)
}

pub fn checkout(repo: &Repository, head: Reference, dry_run: bool) -> Result<()> {
    let head_refname = head.name().context("non-utf8 head ref name")?;
    if !dry_run {
        git(repo, &["checkout", head_refname], Level::Info)
    } else {
        info!("> git checkout {} (dry-run)", head_refname);

        println!("Note: switching to '{}' (dry run)", head_refname);
        println!("You are in 'detached HED' state... blah blah...");
        let commit = head.peel_to_commit()?;
        let message = commit.message().context("non-utf8 head ref name")?;
        println!(
            "HEAD is now at {} {} (dry run)",
            &commit.id().to_string()[..7],
            message.lines().next().unwrap_or_default()
        );
        Ok(())
    }
}

pub fn branch_delete(repo: &Repository, branch_names: &[&str], dry_run: bool) -> Result<()> {
    let mut args = vec!["branch", "--delete", "--force"];
    args.extend(branch_names);
    if !dry_run {
        git(repo, &args, Level::Info)
    } else {
        for branch_name in branch_names {
            info!("> git {} (dry-run)", args.join(" "));
            println!("Delete branch {} (dry run).", branch_name);
        }
        Ok(())
    }
}

pub fn push_delete(
    repo: &Repository,
    remote_name: &str,
    remote_refnames: &[&str],
    dry_run: bool,
) -> Result<()> {
    let mut command = vec!["push", "--delete"];
    if dry_run {
        command.push("--dry-run");
    }
    command.push(remote_name);
    command.extend(remote_refnames);
    git(repo, &command, Level::Trace)
}
