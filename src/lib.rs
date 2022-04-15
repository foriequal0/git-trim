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
use git2::{Config as GitConfig, Error as GitError, ErrorCode, Repository};
use log::*;

use crate::args::DeleteFilter;
use crate::branch::RemoteTrackingBranchStatus;
pub use crate::branch::{
    LocalBranch, Refname, RemoteBranch, RemoteBranchError, RemoteTrackingBranch,
};
use crate::core::{
    get_direct_fetch_branches, get_non_tracking_local_branches,
    get_non_upstream_remote_tracking_branches, get_remote_heads, get_tracking_branches, Classifier,
    DirectFetchClassificationRequest, NonTrackingBranchClassificationRequest,
    NonUpstreamBranchClassificationRequest, TrackingBranchClassificationRequest,
};
pub use crate::core::{ClassifiedBranch, SkipSuggestion, TrimPlan};
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
    pub protected_patterns: Vec<&'a str>,
    pub delete: DeleteFilter,
    pub detach: bool,
}

pub fn get_trim_plan(git: &Git, param: &PlanParam) -> Result<TrimPlan> {
    let bases = resolve_bases(&git.repo, &git.config, &param.bases)?;
    let base_upstreams: Vec<_> = bases
        .iter()
        .map(|b| match b {
            BaseSpec::Local { upstream, .. } => upstream.clone(),
            BaseSpec::Remote { remote, .. } => remote.clone(),
        })
        .collect();
    trace!("bases: {:#?}", bases);

    let tracking_branches = get_tracking_branches(git)?;
    debug!("tracking_branches: {:#?}", tracking_branches);

    let direct_fetch_branches = get_direct_fetch_branches(git)?;
    debug!("direct_fetch_branches: {:#?}", direct_fetch_branches);

    let non_tracking_branches = get_non_tracking_local_branches(git)?;
    debug!("non_tracking_branches: {:#?}", non_tracking_branches);

    let non_upstream_branches = get_non_upstream_remote_tracking_branches(git)?;
    debug!("non_upstream_branches: {:#?}", non_upstream_branches);

    let remote_heads = if param.delete.scan_tracking() {
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
    let mut skipped = HashMap::new();

    info!("Enqueue classification requests");
    if param.delete.scan_tracking() {
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
    } else {
        for (local, upstream) in &tracking_branches {
            if let Some(upstream) = upstream {
                let remote = upstream.to_remote_branch(&git.repo)?.remote;
                let suggestion = SkipSuggestion::TrackingRemote(remote);
                skipped.insert(local.refname.clone(), suggestion.clone());
                skipped.insert(upstream.refname.clone(), suggestion.clone());
            } else {
                skipped.insert(local.refname.clone(), SkipSuggestion::Tracking);
            }
        }

        for (local, _) in &direct_fetch_branches {
            skipped.insert(local.refname.clone(), SkipSuggestion::Tracking);
        }
    }

    if param.delete.scan_non_tracking_local() {
        for base in &base_upstreams {
            for local in &non_tracking_branches {
                classifier.queue_request(NonTrackingBranchClassificationRequest { base, local });
            }
        }
    } else {
        for local in &non_tracking_branches {
            skipped.insert(local.refname.clone(), SkipSuggestion::NonTracking);
        }
    }

    for base in &base_upstreams {
        for remote_tracking in &non_upstream_branches {
            let remote = remote_tracking.to_remote_branch(&git.repo)?;
            if param.delete.scan_non_upstream_remote(&remote.remote) {
                classifier.queue_request(NonUpstreamBranchClassificationRequest {
                    base,
                    remote: remote_tracking,
                });
            } else {
                let remote = remote_tracking.to_remote_branch(&git.repo)?.remote;
                skipped.insert(
                    remote_tracking.refname.clone(),
                    SkipSuggestion::NonUpstream(remote),
                );
            }
        }
    }

    let classifications = classifier.classify()?;

    let mut result = TrimPlan {
        skipped,
        to_delete: HashSet::new(),
        preserved: Vec::new(),
    };
    for classification in classifications {
        result.to_delete.extend(classification.result);
    }

    result.preserve_bases(&git.repo, &git.config, &bases)?;
    result.preserve_protected(&git.repo, &param.protected_patterns)?;
    result.preserve_non_heads_remotes(&git.repo)?;
    result.preserve_worktree(&git.repo)?;
    result.apply_delete_range_filter(&git.repo, &param.delete)?;

    if !param.detach {
        result.adjust_not_to_detach(&git.repo)?;
    }

    Ok(result)
}

#[derive(Debug)]
pub(crate) enum BaseSpec<'a> {
    Local {
        #[allow(dead_code)]
        pattern: &'a str,
        local: LocalBranch,
        upstream: RemoteTrackingBranch,
    },
    Remote {
        pattern: &'a str,
        remote: RemoteTrackingBranch,
    },
}

impl<'a> BaseSpec<'a> {
    fn is_local(&self, branch: &LocalBranch) -> bool {
        matches!(self, BaseSpec::Local { local, .. } if local == branch)
    }

    fn covers_remote(&self, refname: &str) -> bool {
        match self {
            BaseSpec::Local { upstream, .. } if upstream.refname() == refname => true,
            BaseSpec::Remote { remote, .. } if remote.refname() == refname => true,
            _ => false,
        }
    }

    fn remote_pattern(&self, refname: &str) -> Option<&str> {
        match self {
            BaseSpec::Remote { pattern, remote } if remote.refname() == refname => Some(pattern),
            _ => None,
        }
    }
}

pub(crate) fn resolve_bases<'a>(
    repo: &Repository,
    config: &GitConfig,
    bases: &[&'a str],
) -> Result<Vec<BaseSpec<'a>>> {
    let mut result = Vec::new();
    for base in bases {
        let reference = match repo.resolve_reference_from_short_name(base) {
            Ok(reference) => reference,
            Err(err) if err.code() == ErrorCode::NotFound => continue,
            Err(err) => return Err(err.into()),
        };

        if reference.is_branch() {
            let local = LocalBranch::try_from(&reference)?;
            if let RemoteTrackingBranchStatus::Exists(upstream) =
                local.fetch_upstream(repo, config)?
            {
                result.push(BaseSpec::Local {
                    pattern: base,
                    local,
                    upstream,
                })
            }
        } else {
            let remote = RemoteTrackingBranch::try_from(&reference)?;
            result.push(BaseSpec::Remote {
                pattern: base,
                remote,
            })
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
