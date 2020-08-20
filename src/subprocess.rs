use std::collections::{HashMap, HashSet};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use git2::{Config, Reference, Repository};
use log::*;

use crate::branch::{LocalBranch, RemoteBranch, RemoteTrackingBranch, RemoteTrackingBranchStatus};

fn git(repo: &Repository, args: &[&str], level: log::Level) -> Result<()> {
    let workdir = repo.workdir().context("Bare repository is not supported")?;
    let workdir = workdir.to_str().context("non utf-8 workdir")?;
    log!(level, "> git {}", args.join(" "));

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
    log!(level, "> git {}", args.join(" "));

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
        trace!("| {}", line);
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

/// Get whether there any commits are not in the `base` from the `commit`
/// `git rev-list --cherry-pick --right-only --no-merges -n1 <base>..<commit>`
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

/// Get branches that are merged with merge commit.
/// `git branch --format '%(refname)' --merged <base>`
pub fn get_noff_merged_locals(
    repo: &Repository,
    config: &Config,
    bases: &[RemoteTrackingBranch],
) -> Result<HashSet<LocalBranch>> {
    let mut result = HashSet::new();
    for base in bases {
        let refnames = git_output(
            repo,
            &[
                "branch",
                "--format",
                "%(refname)",
                "--merged",
                &base.refname,
            ],
            Level::Trace,
        )?;
        for refname in refnames.lines() {
            if !refnames.starts_with("refs/") {
                // Detached HEAD is printed as '(HEAD detached at 1234abc)'
                continue;
            }
            let branch = LocalBranch::new(refname);
            let upstream = branch.fetch_upstream(repo, config)?;
            if let RemoteTrackingBranchStatus::Exists(upstream) = upstream {
                if base == &upstream {
                    continue;
                }
            }
            let reference = repo.find_reference(&refname)?;
            if reference.symbolic_target().is_some() {
                continue;
            }
            result.insert(branch);
        }
    }
    Ok(result)
}

/// Get remote tracking branches that are merged with merge commit.
/// `git branch --format '%(refname)' --remote --merged <base>`
pub fn get_noff_merged_remotes(
    repo: &Repository,
    bases: &[RemoteTrackingBranch],
) -> Result<HashSet<RemoteTrackingBranch>> {
    let mut result = HashSet::new();
    for base in bases {
        let refnames = git_output(
            repo,
            &[
                "branch",
                "--format",
                "%(refname)",
                "--remote",
                "--merged",
                &base.refname,
            ],
            Level::Trace,
        )?;
        for refname in refnames.lines() {
            let branch = RemoteTrackingBranch::new(refname);
            if base == &branch {
                continue;
            }
            let reference = repo.find_reference(&refname)?;
            if reference.symbolic_target().is_some() {
                continue;
            }
            result.insert(branch);
        }
    }
    Ok(result)
}

pub struct RemoteHead {
    pub remote: String,
    pub refname: String,
    pub commit: String,
}

pub fn ls_remote_heads(repo: &Repository, remote_name: &str) -> Result<Vec<RemoteHead>> {
    let mut result = Vec::new();
    for line in git_output(repo, &["ls-remote", "--heads", remote_name], Level::Trace)?.lines() {
        let records = line.split_whitespace().collect::<Vec<_>>();
        let commit = records[0].to_string();
        let refname = records[1].to_string();
        result.push(RemoteHead {
            remote: remote_name.to_owned(),
            refname,
            commit,
        });
    }
    Ok(result)
}

pub fn ls_remote_head(repo: &Repository, remote_name: &str) -> Result<RemoteHead> {
    let command = &["ls-remote", "--symref", remote_name, "HEAD"];
    let lines = git_output(repo, command, Level::Trace)?;
    let mut refname = None;
    let mut commit = None;
    for line in lines.lines() {
        if line.starts_with("ref: ") {
            refname = Some(
                line["ref: ".len()..line.len() - "HEAD".len()]
                    .trim()
                    .to_owned(),
            )
        } else {
            commit = line.split_whitespace().next().map(|x| x.to_owned());
        }
    }
    if let (Some(refname), Some(commit)) = (refname, commit) {
        Ok(RemoteHead {
            remote: remote_name.to_owned(),
            refname,
            commit,
        })
    } else {
        Err(anyhow::anyhow!("HEAD not found on {}", remote_name))
    }
}

/// Get worktrees and its paths without HEAD
pub fn get_worktrees(repo: &Repository) -> Result<HashMap<LocalBranch, String>> {
    // TODO: `libgit2` has `git2_worktree_*` APIs. However it is not ported to `git2`. Use subprocess directly.
    let mut result = HashMap::new();
    let mut worktree = None;
    let mut branch = None;
    for line in git_output(repo, &["worktree", "list", "--porcelain"], Level::Trace)?.lines() {
        if line.starts_with("worktree ") {
            worktree = Some(line["worktree ".len()..].to_owned());
        } else if line.starts_with("branch ") {
            branch = Some(LocalBranch::new(&line["branch ".len()..]));
        } else if line.is_empty() {
            if let (Some(worktree), Some(branch)) = (worktree.take(), branch.take()) {
                result.insert(branch, worktree);
            }
        }
    }

    if let (Some(worktree), Some(branch)) = (worktree.take(), branch.take()) {
        result.insert(branch, worktree);
    }

    let head = repo.head()?;
    if head.is_branch() {
        let head_branch = LocalBranch::new(head.name().context("non-utf8 head branch name")?);
        result.remove(&head_branch);
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

pub fn branch_delete(repo: &Repository, branches: &[&LocalBranch], dry_run: bool) -> Result<()> {
    let mut args = vec!["branch", "--delete", "--force"];
    let mut branch_names = Vec::new();
    for branch in branches {
        let reference = repo.find_reference(&branch.refname)?;
        assert!(reference.is_branch());
        let branch_name = reference.shorthand().context("non utf-8 branch name")?;
        branch_names.push(branch_name.to_owned());
    }
    args.extend(branch_names.iter().map(|x| x.as_str()));

    if !dry_run {
        git(repo, &args, Level::Info)
    } else {
        info!("> git {} (dry-run)", args.join(" "));
        for branch_name in branch_names {
            println!("Delete branch {} (dry run).", branch_name);
        }
        Ok(())
    }
}

pub fn push_delete(
    repo: &Repository,
    remote_name: &str,
    remote_branches: &[&RemoteBranch],
    dry_run: bool,
) -> Result<()> {
    assert!(remote_branches
        .iter()
        .all(|branch| branch.remote == remote_name));
    let mut command = vec!["push", "--delete"];
    if dry_run {
        command.push("--dry-run");
    }
    command.push(remote_name);
    for remote_branch in remote_branches {
        command.push(&remote_branch.refname);
    }
    git(repo, &command, Level::Trace)
}
