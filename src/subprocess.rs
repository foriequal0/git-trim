use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use git2::{BranchType, Config, Reference, Repository};
use log::*;

use crate::config::get_remote;
use crate::remote_ref::get_fetch_remote_ref;

fn git(repo: &Repository, args: &[&str]) -> Result<()> {
    let workdir = repo.workdir().context("Bare repository is not supported")?;
    let workdir = workdir.to_str().context("non utf-8 workdir")?;
    info!("> git -C {} {}", workdir, args.join(" "));

    let mut cd_args = vec!["-C", workdir];
    cd_args.extend_from_slice(args);
    let exit_status = Command::new("git").args(cd_args).status()?;
    if !exit_status.success() {
        Err(std::io::Error::from_raw_os_error(exit_status.code().unwrap_or(-1)).into())
    } else {
        Ok(())
    }
}

fn git_output(repo: &Repository, args: &[&str]) -> Result<String> {
    let workdir = repo.workdir().context("Bare repository is not supported")?;
    let workdir = workdir.to_str().context("non utf-8 workdir")?;
    info!("> git -C {} {}", workdir, args.join(" "));

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
    trace!("output:");
    for line in str.lines() {
        trace!("{}", line);
    }
    Ok(str.to_string())
}

pub fn remote_update(repo: &Repository) -> Result<()> {
    git(repo, &["remote", "update", "--prune"])
}

pub fn is_merged(repo: &Repository, base: &str, branch: &str) -> Result<bool> {
    let merge_base = git_output(&repo, &["merge-base", base, branch])?;
    Ok(is_merged_by_rev_list(repo, base, branch)?
        || is_squash_merged(repo, &merge_base, base, branch)?)
}

fn is_merged_by_rev_list(repo: &Repository, base: &str, branch: &str) -> Result<bool> {
    let range = format!("{}...{}", base, branch);
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
    )?;

    // empty output means there aren't any revs that are not applied to the base.
    Ok(output.is_empty())
}

/// Source: https://stackoverflow.com/a/56026209
fn is_squash_merged(repo: &Repository, merge_base: &str, base: &str, branch: &str) -> Result<bool> {
    let tree = git_output(repo, &["rev-parse", &format!("{}^{{tree}}", branch)])?;
    let dangling_commit = git_output(
        repo,
        &[
            "commit-tree",
            &tree,
            "-p",
            &merge_base,
            "-m",
            "git-trim: squash merge test",
        ],
    )?;

    is_merged_by_rev_list(repo, base, &dangling_commit)
}

pub fn get_noff_merged_locals(
    repo: &Repository,
    config: &Config,
    base_remote_ref: &str,
) -> Result<Vec<String>> {
    let output = git_output(
        repo,
        &[
            "branch",
            "--format",
            "%(refname:short)",
            "--merged",
            base_remote_ref,
        ],
    )?;
    let mut result = Vec::new();
    for refname in output.lines() {
        trace!("refname: {}", refname);
        if get_remote(config, refname)?.is_implicit() {
            trace!("skip: it is not a tracking branch");
            continue;
        }
        let remote_ref = get_fetch_remote_ref(repo, config, refname)?;
        if Some(base_remote_ref) == remote_ref.as_deref() {
            trace!("skip: {} tracks {}", refname, base_remote_ref);
            continue;
        }
        let branch = repo.find_branch(&refname, BranchType::Local)?;
        if branch.get().symbolic_target().is_some() {
            trace!("skip: it is symbolic");
            continue;
        }
        let branch_name = branch.name()?.context("no utf-8 branch name")?.to_string();
        trace!("noff merged local: it is merged to {}", base_remote_ref);
        result.push(branch_name);
    }
    Ok(result)
}

pub fn checkout(repo: &Repository, head: Reference, dry_run: bool) -> Result<()> {
    let head_refname = head.name().context("non-utf8 head ref name")?;
    if !dry_run {
        git(repo, &["checkout", head_refname])
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

pub fn branch_delete(repo: &Repository, branches: &[&str], dry_run: bool) -> Result<()> {
    let mut args = vec!["branch", "--delete", "--force"];
    args.extend(branches);
    if !dry_run {
        git(repo, &args)
    } else {
        for branch in branches {
            info!("> git {} (dry-run)", args.join(" "));
            println!("Delete branch {} (dry run).", branch);
        }
        Ok(())
    }
}

pub fn push_delete(
    repo: &Repository,
    remote_name: &str,
    remote_refnames: &[String],
    dry_run: bool,
) -> Result<()> {
    let mut command = vec!["push", "--delete"];
    if dry_run {
        command.push("--dry-run");
    }
    command.push(remote_name);
    command.extend(remote_refnames.iter().map(String::as_str));
    git(repo, &command)
}
