pub mod args;
mod branch;
pub mod config;
mod core;
mod merge_tracker;
mod simple_glob;
mod subprocess;
mod util;

use std::collections::{HashMap, HashSet};
use std::convert::TryFrom;

use anyhow::{Context, Result};
use git2::{BranchType, Config as GitConfig, Error as GitError, ErrorCode, Repository};
use glob::Pattern;
use log::*;

use crate::args::{DeleteFilter, ScanFilter};
use crate::branch::RemoteTrackingBranchStatus;
pub use crate::branch::{LocalBranch, RemoteBranch, RemoteBranchError, RemoteTrackingBranch};
use crate::core::{
    get_direct_fetch_branches, get_non_tracking_local_branches,
    get_non_upstream_remote_tracking_branches, get_remote_heads, get_tracking_branches, Classifier,
    DirectFetchClassificationRequest, NonTrackingBranchClassificationRequest,
    NonUpstreamBranchClassificationRequest, TrackingBranchClassificationRequest,
};
pub use crate::core::{ClassifiedBranch, TrimPlan};
use crate::merge_tracker::MergeTracker;
pub use crate::subprocess::{ls_remote_head, remote_update, RemoteHead};
pub use crate::util::ForceSendSync;

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
    pub scan: ScanFilter,
    pub delete: DeleteFilter,
    pub detach: bool,
}

pub fn get_trim_plan(git: &Git, param: &PlanParam) -> Result<TrimPlan> {
    let base_refs = normalize_refs(&git.repo, &param.bases)?;
    let base_upstreams = resolve_base_upstreams(&git.repo, &git.config, &base_refs)?;
    let protected_refs = resolve_protected_refs(&git.repo, &git.config, &param.protected_branches)?;
    trace!("base_upstreams: {:#?}", base_upstreams);
    trace!("protected_refs: {:#?}", protected_refs);

    let tracking_branches = get_tracking_branches(git, &base_upstreams)?;
    debug!("tracking_branches: {:#?}", tracking_branches);

    let direct_fetch_branches = get_direct_fetch_branches(git, &base_refs)?;
    debug!("direct_fetch_branches: {:#?}", direct_fetch_branches);

    let non_tracking_branches = get_non_tracking_local_branches(git, &base_refs)?;
    debug!("non_tracking_branches: {:#?}", non_tracking_branches);

    let non_upstream_branches = get_non_upstream_remote_tracking_branches(git, &base_upstreams)?;
    debug!("non_upstream_branches: {:#?}", non_upstream_branches);

    let remote_heads = if param.scan.scan_tracking() {
        let remotes: Vec<_> = direct_fetch_branches
            .iter()
            .map(|(_, r)| r.clone())
            .collect();
        get_remote_heads(git, &remotes)?
    } else {
        Vec::new()
    };
    debug!("remote_heads: {:#?}", remote_heads);

    let merge_tracker = MergeTracker::with_base_upstreams(&git.repo, &git.config, &base_upstreams)?;
    let mut classifier = Classifier::new(git, &merge_tracker);

    info!("Enqueue classification requests");
    if param.scan.scan_tracking() {
        for (local, upstream) in &tracking_branches {
            for base in &base_upstreams {
                classifier.queue_request(TrackingBranchClassificationRequest {
                    base,
                    local,
                    upstream: upstream.as_ref(),
                });
            }
        }

        for (local, remote) in &direct_fetch_branches {
            for base in &base_upstreams {
                classifier.queue_request_with_context(
                    DirectFetchClassificationRequest {
                        base,
                        local,
                        remote,
                    },
                    &remote_heads,
                );
            }
        }
    }

    if param.scan.scan_non_tracking_local() {
        for base in &base_upstreams {
            for local in &non_tracking_branches {
                classifier.queue_request(NonTrackingBranchClassificationRequest { base, local });
            }
        }
    }

    for base in &base_upstreams {
        for remote_tracking in &non_upstream_branches {
            let remote = remote_tracking.to_remote_branch(&git.repo)?;
            if param.scan.scan_non_upstream_remote(&remote.remote) {
                classifier.queue_request(NonUpstreamBranchClassificationRequest {
                    base,
                    remote: remote_tracking,
                });
            }
        }
    }

    let classifications = classifier.classify()?;

    let mut result = TrimPlan {
        to_delete: HashSet::new(),
        preserved: Vec::new(),
    };
    for classification in classifications {
        result.to_delete.extend(classification.result);
    }

    let base_and_upstream_refs =
        resolve_base_and_upstream_refs(&git.repo, &git.config, &base_refs)?;
    result.preserve(&base_and_upstream_refs, "base")?;
    result.preserve(&protected_refs, "protected")?;
    result.preserve_non_heads_remotes(&git.repo)?;
    result.preserve_worktree(&git.repo)?;
    result.apply_delete_filter(&git.repo, &param.delete)?;

    if !param.detach {
        result.adjust_not_to_detach(&git.repo)?;
    }

    Ok(result)
}

fn normalize_refs(repo: &Repository, names: &[&str]) -> Result<Vec<String>> {
    let mut result = Vec::new();
    for name in names {
        let refname = match repo.resolve_reference_from_short_name(name) {
            Ok(reference) => reference.name().context("non utf-8 branch ref")?.to_owned(),
            Err(err) if err.code() == ErrorCode::NotFound => continue,
            Err(err) => return Err(err.into()),
        };
        result.push(refname);
    }
    Ok(result)
}

fn resolve_base_and_upstream_refs(
    repo: &Repository,
    config: &GitConfig,
    bases: &[String],
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
            if let RemoteTrackingBranchStatus::Exists(upstream) =
                branch.fetch_upstream(repo, config)?
            {
                result.insert(upstream.refname);
            }
        }
    }
    Ok(result)
}

fn resolve_base_upstreams(
    repo: &Repository,
    config: &GitConfig,
    bases: &[String],
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
                if let RemoteTrackingBranchStatus::Exists(upstream) =
                    branch.fetch_upstream(repo, config)?
                {
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
                if let RemoteTrackingBranchStatus::Exists(upstream) =
                    branch.fetch_upstream(repo, config)?
                {
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
    remote_branches: &[RemoteBranch],
    dry_run: bool,
) -> Result<()> {
    if remote_branches.is_empty() {
        return Ok(());
    }
    let mut per_remote = HashMap::new();
    for remote_branch in remote_branches {
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
