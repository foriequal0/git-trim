pub mod args;
mod branch;
pub mod config;
mod core;
mod simple_glob;
mod subprocess;
mod util;

use std::collections::{HashMap, HashSet};
use std::convert::TryFrom;

use anyhow::{Context, Result};
use git2::{BranchType, Config as GitConfig, Error as GitError, ErrorCode, Repository};
use glob::Pattern;
use log::*;
use rayon::prelude::*;

use crate::args::DeleteFilter;
use crate::branch::{get_fetch_upstream, get_remote_entry};
pub use crate::branch::{LocalBranch, RemoteBranch, RemoteBranchError, RemoteTrackingBranch};
use crate::core::MergeTracker;
pub use crate::core::{ClassifiedBranch, TrimPlan};
use crate::subprocess::ls_remote_heads;
pub use crate::subprocess::remote_update;
use crate::util::ForceSendSync;

pub struct Git {
    pub repo: Repository,
    pub config: GitConfig,
}

impl TryFrom<Repository> for Git {
    type Error = GitError;

    fn try_from(repo: Repository) -> Result<Self, Self::Error> {
        let config = repo.config()?.snapshot()?;
        Ok(Self { repo, config })
    }
}

pub struct PlanParam<'a> {
    pub bases: Vec<&'a str>,
    pub protected_branches: HashSet<&'a str>,
    pub filter: DeleteFilter,
    pub detach: bool,
}

#[allow(clippy::cognitive_complexity, clippy::implicit_hasher)]
pub fn get_trim_plan(git: &Git, param: &PlanParam) -> Result<TrimPlan> {
    let base_upstreams = resolve_base_upstream(&git.repo, &git.config, &param.bases)?;
    let protected_refs = resolve_protected_refs(&git.repo, &git.config, &param.protected_branches)?;
    trace!("base_upstreams: {:#?}", base_upstreams);
    trace!("protected_refs: {:#?}", protected_refs);

    let merge_tracker = MergeTracker::new();
    for base_upstream in &base_upstreams {
        merge_tracker.track(&git.repo, &base_upstream.refname)?;
    }
    for merged_locals in
        subprocess::get_noff_merged_locals(&git.repo, &git.config, &base_upstreams)?
    {
        merge_tracker.track(&git.repo, &merged_locals.refname)?;
    }
    for merged_remotes in subprocess::get_noff_merged_remotes(&git.repo, &base_upstreams)? {
        merge_tracker.track(&git.repo, &merged_remotes.refname)?;
    }

    let mut base_and_branch_to_compare = Vec::new();
    let mut remote_urls = Vec::new();
    for branch in git.repo.branches(Some(BranchType::Local))? {
        let branch = LocalBranch::try_from(&branch?.0)?;
        let fetch_upstream = get_fetch_upstream(&git.repo, &git.config, &branch)?;
        debug!("Branch ref: {:?}", branch);
        debug!("Fetch upstream: {:?}", fetch_upstream);

        let config_remote = if let Some(remote) = config::get_remote_raw(&git.config, &branch)? {
            remote
        } else {
            debug!(
                "Skip: the branch doesn't have a tracking remote: {:?}",
                branch
            );
            continue;
        };

        if get_remote_entry(&git.repo, &config_remote)?.is_none() {
            debug!(
                "The branch's remote is assumed to be an URL: {}",
                config_remote.as_str()
            );
            remote_urls.push(config_remote.to_string());
        }
        if let Some(upstream) = &fetch_upstream {
            if base_upstreams.contains(&upstream) {
                debug!("Skip: the branch tracks the base: {:?}", branch);
                continue;
            }
        }

        for base in &base_upstreams {
            base_and_branch_to_compare.push((base, branch.clone()));
        }
    }

    let remote_heads_per_url = remote_urls
        .into_par_iter()
        .map({
            let git = ForceSendSync::new(git);
            move |remote_url| {
                ls_remote_heads(&git.repo, &remote_url)
                    .with_context(|| format!("remote_url={}", remote_url))
                    .map(|remote_heads| (remote_url.to_string(), remote_heads))
            }
        })
        .collect::<Result<HashMap<String, HashSet<String>>, _>>()?;

    info!("Start classify:");
    let classifications = base_and_branch_to_compare
        .into_par_iter()
        .map({
            // git's fields are semantically Send + Sync in the `classify`.
            // They are read only in `classify` function.
            // It is denoted that it is safe in that case
            // https://github.com/libgit2/libgit2/blob/master/docs/threading.md#sharing-objects
            let git = ForceSendSync::new(git);
            move |(base, branch)| {
                core::classify(git, &merge_tracker, &remote_heads_per_url, base, &branch)
                    .with_context(|| format!("base={:?}, branch={:?}", base, branch))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut delete = HashSet::new();
    for classification in classifications.into_iter() {
        debug!("branch: {:?}", classification.local);
        trace!("fetch: {:?}", classification.fetch);
        debug!("message: {:?}", classification.messages);
        delete.extend(classification.result.into_iter());
    }

    let mut result = TrimPlan {
        to_delete: delete,
        preserved: Vec::new(),
    };
    let base_refs = resolve_base_refs(&git.repo, &git.config, &param.bases)?;
    result.preserve(&git.repo, &base_refs, "a base")?;
    result.preserve(&git.repo, &protected_refs, "a protected")?;
    result.preserve_non_heads_remotes();
    result.preserve_worktree(&git.repo)?;
    result.apply_filter(&param.filter)?;

    if !param.detach {
        result.adjust_not_to_detach(&git.repo)?;
    }

    Ok(result)
}

/// if there are following references:
/// refs/heads/master
/// refs/remotes/origin/master
/// refs/remotes/upstream/master
/// and master's upstreams:
/// fetch: upstream/release-v1.x
/// push: origin/release-v1.x
///
/// master
/// refs/heads/master because it shouldn't be removed from the local
/// refs/remotes/origin/master because it shouldn't be removed from the push remote
/// refs/remotes/upstream/master because it shouldn't be remvoed from the fetch remote
fn resolve_base_refs(
    repo: &Repository,
    config: &GitConfig,
    bases: &[&str],
) -> Result<HashSet<String>> {
    let mut result = HashSet::new();
    for base in bases {
        let reference = match repo.resolve_reference_from_short_name(base) {
            Ok(reference) => {
                let refname = reference.name().context("non utf-8 base branch ref")?;
                result.insert((*refname).to_string());
                reference
            }
            Err(err) if err.code() == ErrorCode::NotFound => continue,
            Err(err) => return Err(err.into()),
        };

        if reference.is_branch() {
            let refname = reference.name().context("non utf-8 base refname")?;
            let branch = LocalBranch::new(refname);
            if let Some(upstream) = get_fetch_upstream(repo, config, &branch)? {
                result.insert(upstream.refname);
            }
        }
    }
    Ok(result)
}

fn resolve_base_upstream(
    repo: &Repository,
    config: &GitConfig,
    bases: &[&str],
) -> Result<Vec<RemoteTrackingBranch>> {
    let mut result = Vec::new();
    for base in bases {
        if base.starts_with("refs/remotes/") {
            if repo.find_reference(base).is_ok() {
                result.push(RemoteTrackingBranch::new(base));
                continue;
            } else {
                // The tracking remote branch is not fetched.
                // Just skip.
            }
        } else {
            let reference = match repo.resolve_reference_from_short_name(base) {
                Ok(reference) => reference,
                Err(err) if err.code() == ErrorCode::NotFound => continue,
                Err(err) => return Err(err.into()),
            };

            if let Ok(branch) = LocalBranch::try_from(&reference) {
                if let Some(upstream) = get_fetch_upstream(repo, config, &branch)? {
                    result.push(upstream);
                    continue;
                }
            // We compares this functions's results with other branches.
            // Our concern is whether the branches are safe to delete.
            // Safe means we can be fetch the entire content of the branches from the base.
            // So we skips get_push_upstream since we don't fetch from them.
            } else if reference.is_remote() {
                // match "origin/master -> refs/remotes/origin/master"
                let refname = reference.name().context("non-utf8 reference name")?;
                result.push(RemoteTrackingBranch::new(refname));
                continue;
            }
        }
    }
    Ok(result)
}

/// protected branch patterns
/// if there are following references:
/// refs/heads/release-v1.x
/// refs/remotes/origin/release-v1.x
/// refs/remotes/upstream/release-v1.x
/// and release-v1.x tracks upstream/release-v1.x
///
/// release-*
/// -> refs/heads/release-v1.x,
///    refs/remotes/upstream/release-v1.x,
/// origin/release-*
/// -> refs/remotes/origin/release-v1.x
/// refs/heads/release-*
/// -> refs/heads/release-v1.x
/// refs/remotes/origin/release-*
/// -> refs/remotes/origin/release-v1.x
#[allow(clippy::implicit_hasher)]
fn resolve_protected_refs(
    repo: &Repository,
    config: &GitConfig,
    protected_branches: &HashSet<&str>,
) -> Result<HashSet<String>> {
    let mut result = HashSet::default();
    for protected_branch in protected_branches {
        for reference in repo.references_glob(protected_branch)? {
            let reference = reference?;
            let refname = reference.name().context("non utf-8 refname")?;
            result.insert(refname.to_string());
        }
        for reference in repo.references_glob(&format!("refs/remotes/{}", protected_branch))? {
            let reference = reference?;
            let refname = reference.name().context("non utf-8 refname")?;
            result.insert(refname.to_string());
        }
        for branch in repo.branches(Some(BranchType::Local))? {
            let (branch, _) = branch?;
            let branch_name = branch.name()?.context("non utf-8 branch name")?;
            if Pattern::new(protected_branch)?.matches(branch_name) {
                let branch = LocalBranch::try_from(&branch)?;
                result.insert(branch.refname.to_string());
                if let Some(upstream) = get_fetch_upstream(repo, config, &branch)? {
                    result.insert(upstream.refname);
                }
            }
        }
    }
    Ok(result)
}

pub fn delete_local_branches(
    repo: &Repository,
    branches: &[&LocalBranch],
    dry_run: bool,
) -> Result<()> {
    if branches.is_empty() {
        return Ok(());
    }

    let detach_to = if repo.head_detached()? {
        None
    } else {
        let head = repo.head()?;
        let head_refname = head.name().context("non-utf8 head ref name")?;
        if branches.iter().any(|branch| branch.refname == head_refname) {
            Some(head)
        } else {
            None
        }
    };

    if let Some(head) = detach_to {
        subprocess::checkout(repo, head, dry_run)?;
    }
    subprocess::branch_delete(repo, branches, dry_run)?;

    Ok(())
}

pub fn delete_remote_branches(
    repo: &Repository,
    remote_branches: &[&RemoteBranch],
    dry_run: bool,
) -> Result<()> {
    if remote_branches.is_empty() {
        return Ok(());
    }
    let mut per_remote = HashMap::new();
    for remote_branch in remote_branches.iter().copied() {
        let entry = per_remote
            .entry(&remote_branch.remote)
            .or_insert_with(Vec::new);
        entry.push(remote_branch);
    }
    for (remote_name, remote_refnames) in per_remote.iter() {
        subprocess::push_delete(repo, remote_name, remote_refnames, dry_run)?;
    }
    Ok(())
}
