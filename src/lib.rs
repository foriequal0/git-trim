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
use crate::branch::{get_fetch_upstream, get_push_upstream, get_remote};
pub use crate::branch::{RemoteBranch, RemoteBranchError, RemoteTrackingBranch};
pub use crate::core::{MergedOrStray, MergedOrStrayAndKeptBacks};
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

pub struct Config<'a> {
    pub bases: Vec<&'a str>,
    pub protected_branches: HashSet<&'a str>,
    pub filter: DeleteFilter,
    pub detach: bool,
}

#[allow(clippy::cognitive_complexity, clippy::implicit_hasher)]
pub fn get_merged_or_stray(git: &Git, config: &Config) -> Result<MergedOrStrayAndKeptBacks> {
    let base_upstreams = resolve_base_upstream(&git.repo, &git.config, &config.bases)?;
    let protected_refs =
        resolve_protected_refs(&git.repo, &git.config, &config.protected_branches)?;
    trace!("base_upstreams: {:#?}", base_upstreams);
    trace!("protected_refs: {:#?}", protected_refs);

    let mut merged_or_stray = MergedOrStray::default();
    // Fast filling ff merged branches
    let noff_merged_locals =
        subprocess::get_noff_merged_locals(&git.repo, &git.config, &base_upstreams)?;
    merged_or_stray
        .merged_locals
        .extend(noff_merged_locals.clone());

    let mut merged_locals = HashSet::new();
    merged_locals.extend(noff_merged_locals);

    let mut base_and_branch_to_compare = Vec::new();
    let mut remote_urls = Vec::new();
    for branch in git.repo.branches(Some(BranchType::Local))? {
        let (branch, _) = branch?;
        let refname = branch.get().name().context("non-utf8 branch ref")?;
        let fetch_upstream = get_fetch_upstream(&git.repo, &git.config, refname)?;
        let push_upstream = get_push_upstream(&git.repo, &git.config, refname)?;
        debug!("Branch ref: {}", refname);
        debug!("Fetch upstream: {:?}", fetch_upstream);
        debug!("Push upstream: {:?}", push_upstream);

        let config_remote = config::get_remote(&git.config, refname)?;
        if config_remote.is_implicit() {
            debug!(
                "Skip: the branch doesn't have a tracking remote: {}",
                refname
            );
            continue;
        }
        if get_remote(&git.repo, &config_remote)?.is_none() {
            debug!(
                "The branch's remote is assumed to be an URL: {}",
                config_remote.as_str()
            );
            remote_urls.push(config_remote.to_string());
        }
        if branch.get().symbolic_target().is_some() {
            debug!("Skip: the branch is a symbolic ref: {}", refname);
            continue;
        }

        if protected_refs.contains(refname) {
            debug!("Skip: the branch is protected branch: {}", refname);
            continue;
        }
        if let Some(upstream) = &fetch_upstream {
            if base_upstreams.contains(&upstream) {
                debug!("Skip: the branch is the base: {}", refname);
                continue;
            }
            if protected_refs.contains(&upstream.refname) {
                debug!("Skip: the branch tracks protected branch: {}", refname);
            }
        }

        for base in &base_upstreams {
            base_and_branch_to_compare.push((base, refname.to_string()));
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

    let classifications = base_and_branch_to_compare
        .into_par_iter()
        .map({
            // git's fields are semantically Send + Sync in the `classify`.
            // They are read only in `classify` function.
            // It is denoted that it is safe in that case
            // https://github.com/libgit2/libgit2/blob/master/docs/threading.md#sharing-objects
            let git = ForceSendSync::new(git);
            move |(base, refname)| {
                core::classify(git, &merged_locals, &remote_heads_per_url, &base, &refname)
                    .with_context(|| format!("base={:?}, refname={}", base, refname))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    for classification in classifications.into_iter() {
        debug!("branch: {:?}", classification.branch);
        trace!("merged: {}", classification.branch_is_merged);
        trace!("fetch: {:?}", classification.fetch);
        trace!("push: {:?}", classification.push);
        debug!("message: {:?}", classification.messages);
        merged_or_stray = merged_or_stray.accumulate(classification.result);
    }

    let mut result = MergedOrStrayAndKeptBacks {
        to_delete: merged_or_stray,
        kept_backs: HashMap::new(),
        kept_back_remotes: HashMap::new(),
    };
    let base_refs = resolve_base_refs(&git.repo, &git.config, &config.bases)?;
    result.keep_base(&git.repo, &base_refs)?;
    result.keep_protected(&git.repo, &protected_refs)?;
    result.keep_non_heads_remotes();
    result.apply_filter(&config.filter)?;

    if !config.detach {
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
            if let Some(upstream) = get_fetch_upstream(repo, config, refname)? {
                result.insert(upstream.refname);
            }

            if let Some(upstream) = get_push_upstream(repo, config, refname)? {
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
            // find "master, refs/heads/master -> refs/remotes/origin/master"
            if let Some(upstream) = get_fetch_upstream(repo, config, base)? {
                result.push(upstream);
                continue;
            }
            // We compares this functions's results with other branches.
            // Our concern is whether the branches are safe to delete.
            // Safe means we can be fetch the entire content of the branches from the base.
            // So we skips get_push_upstream since we don't fetch from them.

            // match "origin/master -> refs/remotes/origin/master"
            if let Ok(remote_ref) = repo.find_reference(&format!("refs/remotes/{}", base)) {
                let refname = remote_ref.name().context("non-utf8 reference name")?;
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
                let reference = branch.into_reference();
                let refname = reference.name().context("non utf-8 ref")?;
                result.insert(refname.to_string());
                if let Some(upstream) = get_fetch_upstream(repo, config, refname)? {
                    result.insert(upstream.refname);
                }
                if let Some(upstream) = get_push_upstream(repo, config, refname)? {
                    result.insert(upstream.refname);
                }
            }
        }
    }
    Ok(result)
}

pub fn delete_local_branches(repo: &Repository, branches: &[&str], dry_run: bool) -> Result<()> {
    if branches.is_empty() {
        return Ok(());
    }

    let detach_to = if repo.head_detached()? {
        None
    } else {
        let head = repo.head()?;
        let head_refname = head.name().context("non-utf8 head ref name")?;
        if branches.contains(&head_refname) {
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
    for remote_branch in remote_branches {
        let entry = per_remote
            .entry(&remote_branch.remote)
            .or_insert_with(Vec::new);
        entry.push(remote_branch.refname.as_str());
    }
    for (remote_name, remote_refnames) in per_remote.iter() {
        subprocess::push_delete(repo, remote_name, remote_refnames, dry_run)?;
    }
    Ok(())
}
