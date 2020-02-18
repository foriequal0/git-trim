use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use git2::{Reference, Repository};
use log::*;

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
    if output.is_empty() {
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Source: https://stackoverflow.com/a/56026209
pub fn is_squash_merged(repo: &Repository, base: &str, branch: &str) -> Result<bool> {
    let merge_base = git_output(repo, &["merge-base", base, branch])?;
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
    is_merged(repo, base, &dangling_commit)
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
