use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use git2::Reference;
use log::*;

fn git(args: &[&str]) -> Result<()> {
    info!("> git {}", args.join(" "));
    let exit_status = Command::new("git").args(args).status()?;
    if !exit_status.success() {
        Err(std::io::Error::from_raw_os_error(exit_status.code().unwrap_or(-1)).into())
    } else {
        Ok(())
    }
}

fn git_output(args: &[&str]) -> Result<String> {
    info!("> git {}", args.join(" "));
    let output = Command::new("git")
        .args(args)
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

pub fn remote_update() -> Result<()> {
    git(&["remote", "update", "--prune"])
}

pub fn is_merged(base: &str, branch: &str) -> Result<bool> {
    let range = format!("{}...{}", base, branch);
    // Is there any revs that are not applied to the base in the branch?
    let output = git_output(&[
        "rev-list",
        "--cherry-pick",
        "--right-only",
        "--no-merges",
        "-n1",
        &range,
    ])?;

    // empty output means there aren't any revs that are not applied to the base.
    if output.is_empty() {
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Source: https://stackoverflow.com/a/56026209
pub fn is_squash_merged(base: &str, branch: &str) -> Result<bool> {
    let merge_base = git_output(&["merge-base", base, branch])?;
    let tree = git_output(&["rev-parse", &format!("{}^{{tree}}", branch)])?;
    let dangling_commit = git_output(&[
        "commit-tree",
        &tree,
        "-p",
        &merge_base,
        "-m",
        "git-trim: squash merge test",
    ])?;
    is_merged(base, &dangling_commit)
}

pub fn checkout(head: Reference, dry_run: bool) -> Result<()> {
    let head_refname = head.name().context("non-utf8 head ref name")?;
    if !dry_run {
        git(&["checkout", head_refname])
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

pub fn branch_delete(branches: &[&str], dry_run: bool) -> Result<()> {
    let mut args = vec!["branch", "--delete", "--force"];
    args.extend(branches);
    if !dry_run {
        git(&args)
    } else {
        for branch in branches {
            info!("> git {} (dry-run)", args.join(" "));
            println!("Delete branch {} (dry run).", branch);
        }
        Ok(())
    }
}

pub fn push_delete(remote_name: &str, remote_refnames: &[String], dry_run: bool) -> Result<()> {
    let mut command = vec!["push", "--delete"];
    if dry_run {
        command.push("--dry-run");
    }
    command.push(remote_name);
    command.extend(remote_refnames.iter().map(String::as_str));
    git(&command)
}
