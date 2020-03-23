pub mod args;
mod branch;
pub mod config;
mod simple_glob;
mod subprocess;

use std::collections::{HashMap, HashSet};
use std::convert::TryFrom;
use std::ops::Deref;

use anyhow::{Context, Result};
use git2::{BranchType, Config as GitConfig, Error as GitError, ErrorCode, Repository};
use glob::Pattern;
use log::*;
use rayon::prelude::*;

use crate::args::DeleteFilter;
use crate::branch::{get_fetch_upstream, get_push_upstream, get_remote};
pub use crate::branch::{RemoteBranch, RemoteBranchError, RemoteTrackingBranch};
use crate::subprocess::ls_remote_heads;
pub use crate::subprocess::remote_update;

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

#[derive(Default, Eq, PartialEq, Debug)]
pub struct MergedOrStray {
    // local branches
    pub merged_locals: HashSet<String>,
    pub stray_locals: HashSet<String>,

    /// remote refs
    pub merged_remotes: HashSet<RemoteBranch>,
    pub stray_remotes: HashSet<RemoteBranch>,
}

impl MergedOrStray {
    fn accumulate(mut self, mut other: Self) -> Self {
        self.merged_locals.extend(other.merged_locals.drain());
        self.stray_locals.extend(other.stray_locals.drain());
        self.merged_remotes.extend(other.merged_remotes.drain());
        self.stray_remotes.extend(other.stray_remotes.drain());

        self
    }

    pub fn locals(&self) -> Vec<&str> {
        self.merged_locals
            .iter()
            .chain(self.stray_locals.iter())
            .map(String::as_str)
            .collect()
    }

    pub fn remotes(&self) -> Vec<&RemoteBranch> {
        self.merged_remotes
            .iter()
            .chain(self.stray_remotes.iter())
            .collect()
    }
}

#[derive(Default, Eq, PartialEq, Debug)]
pub struct MergedOrStrayAndKeptBacks {
    pub to_delete: MergedOrStray,
    pub kept_backs: HashMap<String, Reason>,
    pub kept_back_remotes: HashMap<RemoteBranch, Reason>,
}

#[derive(Clone, Eq, PartialEq, Debug, Ord, PartialOrd)]
pub struct Reason {
    pub original_classification: OriginalClassification,
    pub reason: &'static str,
}

#[derive(Copy, Clone, Eq, PartialEq, Debug, Ord, PartialOrd, Hash)]
pub enum OriginalClassification {
    MergedLocal,
    StrayLocal,
    MergedRemote,
    StrayRemote,
}

impl std::fmt::Display for OriginalClassification {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OriginalClassification::MergedLocal => write!(f, "merged local"),
            OriginalClassification::StrayLocal => write!(f, "stray local"),
            OriginalClassification::MergedRemote => write!(f, "merged remote"),
            OriginalClassification::StrayRemote => write!(f, "stray remote"),
        }
    }
}

impl MergedOrStrayAndKeptBacks {
    fn keep_base(&mut self, repo: &Repository, config: &GitConfig, bases: &[&str]) -> Result<()> {
        let base_refs = resolve_base_refs(repo, config, bases)?;
        trace!("base_refs: {:#?}", base_refs);
        self.kept_backs.extend(keep_branches(
            repo,
            &base_refs,
            Reason {
                original_classification: OriginalClassification::MergedLocal,
                reason: "a base branch",
            },
            &mut self.to_delete.merged_locals,
        )?);
        self.kept_backs.extend(keep_branches(
            repo,
            &base_refs,
            Reason {
                original_classification: OriginalClassification::StrayLocal,
                reason: "a base branch",
            },
            &mut self.to_delete.stray_locals,
        )?);
        self.kept_back_remotes.extend(keep_remote_branches(
            repo,
            &base_refs,
            Reason {
                original_classification: OriginalClassification::MergedRemote,
                reason: "a base branch",
            },
            &mut self.to_delete.merged_remotes,
        )?);
        self.kept_back_remotes.extend(keep_remote_branches(
            repo,
            &base_refs,
            Reason {
                original_classification: OriginalClassification::StrayRemote,
                reason: "a base branch",
            },
            &mut self.to_delete.stray_remotes,
        )?);
        Ok(())
    }

    fn keep_protected(
        &mut self,
        repo: &Repository,
        config: &GitConfig,
        protected_branches: &HashSet<&str>,
    ) -> Result<()> {
        let protected_refs = resolve_protected_refs(repo, config, protected_branches)?;
        trace!("protected_refs: {:#?}", protected_refs);
        self.kept_backs.extend(keep_branches(
            repo,
            &protected_refs,
            Reason {
                original_classification: OriginalClassification::MergedLocal,
                reason: "a protected branch",
            },
            &mut self.to_delete.merged_locals,
        )?);
        self.kept_backs.extend(keep_branches(
            repo,
            &protected_refs,
            Reason {
                original_classification: OriginalClassification::StrayLocal,
                reason: "a protected branch",
            },
            &mut self.to_delete.stray_locals,
        )?);
        self.kept_back_remotes.extend(keep_remote_branches(
            repo,
            &protected_refs,
            Reason {
                original_classification: OriginalClassification::MergedRemote,
                reason: "a protected branch",
            },
            &mut self.to_delete.merged_remotes,
        )?);
        self.kept_back_remotes.extend(keep_remote_branches(
            repo,
            &protected_refs,
            Reason {
                original_classification: OriginalClassification::StrayRemote,
                reason: "a protected branch",
            },
            &mut self.to_delete.stray_remotes,
        )?);
        Ok(())
    }

    fn keep_non_heads_remotes(&mut self) {
        let mut merged_remotes = HashSet::new();
        for remote_branch in &self.to_delete.merged_remotes {
            if remote_branch.refname.starts_with("refs/heads/") {
                merged_remotes.insert(remote_branch.clone());
            } else {
                trace!("filter-out: merged remote ref {}", remote_branch);
                self.kept_back_remotes.insert(
                    remote_branch.clone(),
                    Reason {
                        original_classification: OriginalClassification::MergedRemote,
                        reason: "a non-heads remote branch",
                    },
                );
            }
        }
        self.to_delete.merged_remotes = merged_remotes;

        let mut stray_remotes = HashSet::new();
        for remote_branch in &self.to_delete.stray_remotes {
            if remote_branch.refname.starts_with("refs/heads/") {
                stray_remotes.insert(remote_branch.clone());
            } else {
                trace!("filter-out: stray_remotes remote ref {}", remote_branch);
                self.kept_back_remotes.insert(
                    remote_branch.clone(),
                    Reason {
                        original_classification: OriginalClassification::StrayRemote,
                        reason: "a non-heads remote branch",
                    },
                );
            }
        }
        self.to_delete.stray_remotes = stray_remotes;
    }

    fn apply_filter(&mut self, filter: &DeleteFilter) -> Result<()> {
        trace!("Before filter: {:#?}", self);
        trace!("Applying filter: {:?}", filter);
        if !filter.filter_merged_local() {
            trace!(
                "filter-out: merged local branches {:?}",
                self.to_delete.merged_locals
            );
            self.kept_backs
                .extend(self.to_delete.merged_locals.drain().map(|branch_name| {
                    (
                        branch_name,
                        Reason {
                            original_classification: OriginalClassification::MergedLocal,
                            reason: "out of filter scope",
                        },
                    )
                }));
        }
        if !filter.filter_stray_local() {
            trace!(
                "filter-out: stray local branches {:?}",
                self.to_delete.stray_locals
            );
            self.kept_backs
                .extend(self.to_delete.stray_locals.drain().map(|branch_name| {
                    (
                        branch_name,
                        Reason {
                            original_classification: OriginalClassification::StrayLocal,
                            reason: "out of filter scope",
                        },
                    )
                }));
        }

        let mut merged_remotes = HashSet::new();
        for remote_branch in &self.to_delete.merged_remotes {
            if filter.filter_merged_remote(&remote_branch.remote) {
                merged_remotes.insert(remote_branch.clone());
            } else {
                trace!("filter-out: merged remote ref {}", remote_branch);
                self.kept_back_remotes.insert(
                    remote_branch.clone(),
                    Reason {
                        original_classification: OriginalClassification::MergedRemote,
                        reason: "out of filter scope",
                    },
                );
            }
        }
        self.to_delete.merged_remotes = merged_remotes;

        let mut stray_remotes = HashSet::new();
        for remote_branch in &self.to_delete.stray_remotes {
            if filter.filter_stray_remote(&remote_branch.remote) {
                stray_remotes.insert(remote_branch.clone());
            } else {
                trace!("filter-out: stray_remotes remote ref {}", remote_branch);
                self.kept_back_remotes.insert(
                    remote_branch.clone(),
                    Reason {
                        original_classification: OriginalClassification::StrayRemote,
                        reason: "out of filter scope",
                    },
                );
            }
        }
        self.to_delete.stray_remotes = stray_remotes;

        Ok(())
    }

    fn adjust_not_to_detach(&mut self, repo: &Repository) -> Result<()> {
        if repo.head_detached()? {
            return Ok(());
        }
        let head = repo.head()?;
        let head_name = head.name().context("non-utf8 head ref name")?;
        assert!(head_name.starts_with("refs/heads/"));
        let head_name = &head_name["refs/heads/".len()..];

        if self.to_delete.merged_locals.contains(head_name) {
            self.to_delete.merged_locals.remove(head_name);
            self.kept_backs.insert(
                head_name.to_string(),
                Reason {
                    original_classification: OriginalClassification::MergedLocal,
                    reason: "not to make detached HEAD",
                },
            );
        }
        if self.to_delete.stray_locals.contains(head_name) {
            self.to_delete.stray_locals.remove(head_name);
            self.kept_backs.insert(
                head_name.to_string(),
                Reason {
                    original_classification: OriginalClassification::StrayLocal,
                    reason: "not to make detached HEAD",
                },
            );
        }
        Ok(())
    }
}

fn keep_branches(
    repo: &Repository,
    protected_refs: &HashSet<String>,
    reason: Reason,
    branch_names: &mut HashSet<String>,
) -> Result<HashMap<String, Reason>> {
    let mut kept_back = HashMap::new();
    let mut bag = HashSet::new();
    for branch_name in branch_names.iter() {
        let branch = repo.find_branch(branch_name, BranchType::Local)?;
        let reference = branch.into_reference();
        let refname = reference.name().context("non utf-8 branch ref")?;
        if protected_refs.contains(branch_name) {
            bag.insert(branch_name.to_string());
            bag.insert(refname.to_string());
            kept_back.insert(branch_name.to_string(), reason.clone());
        } else if protected_refs.contains(refname) {
            bag.insert(branch_name.to_string());
            kept_back.insert(refname.to_string(), reason.clone());
        }
    }
    for branch in bag.into_iter() {
        branch_names.remove(&branch);
    }
    Ok(kept_back)
}

fn keep_remote_branches(
    repo: &Repository,
    protected_refs: &HashSet<String>,
    reason: Reason,
    remote_branches: &mut HashSet<RemoteBranch>,
) -> Result<HashMap<RemoteBranch, Reason>> {
    let mut kept_back = HashMap::new();
    for remote_branch in remote_branches.iter() {
        if let Some(remote_tracking) =
            RemoteTrackingBranch::from_remote_branch(repo, remote_branch)?
        {
            if protected_refs.contains(&remote_tracking.refname) {
                kept_back.insert(remote_branch.clone(), reason.clone());
            }
        }
    }
    for remote_branch in kept_back.keys() {
        remote_branches.remove(remote_branch);
    }
    Ok(kept_back)
}

#[allow(clippy::cognitive_complexity, clippy::implicit_hasher)]
pub fn get_merged_or_stray(git: &Git, config: &Config) -> Result<MergedOrStrayAndKeptBacks> {
    let bases = resolve_base_upstream(&git.repo, &git.config, &config.bases)?;
    trace!("base_upstreams: {:#?}", bases);

    let protected_refs =
        resolve_protected_refs(&git.repo, &git.config, &config.protected_branches)?;
    trace!("protected_refs: {:#?}", protected_refs);

    let mut merged_or_stray = MergedOrStray::default();
    // Fast filling ff merged branches
    let noff_merged_locals = subprocess::get_noff_merged_locals(&git.repo, &git.config, &bases)?;
    merged_or_stray
        .merged_locals
        .extend(noff_merged_locals.clone());

    let mut merged_locals = HashSet::new();
    merged_locals.extend(noff_merged_locals);

    let mut base_and_branch_to_compare = Vec::new();
    let mut remote_urls = Vec::new();
    for branch in git.repo.branches(Some(BranchType::Local))? {
        let (branch, _) = branch?;
        let branch_name = branch.name()?.context("non-utf8 branch name")?;
        debug!("Branch: {:?}", branch.name()?);
        let config_remote = config::get_remote(&git.config, branch_name)?;
        if config_remote.is_implicit() {
            debug!(
                "Skip: the branch doesn't have a tracking remote: {:?}",
                branch_name
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

        if protected_refs.contains(branch_name) {
            debug!("Skip: the branch is protected branch: {:?}", branch_name);
            continue;
        }
        if let Some(upstream) = get_fetch_upstream(&git.repo, &git.config, branch_name)? {
            if bases.contains(&upstream) {
                debug!("Skip: the branch is the base: {:?}", branch_name);
                continue;
            }
            if protected_refs.contains(&upstream.refname) {
                debug!(
                    "Skip: the branch tracks protected branch: {:?}",
                    branch_name
                );
            }
        }
        let reference = branch.get();
        if reference.symbolic_target().is_some() {
            debug!("Skip: the branch is a symbolic ref: {:?}", branch_name);
            continue;
        }

        let local_hash = reference.peel_to_commit()?.id();
        if let Some(upstream) = get_fetch_upstream(&git.repo, &git.config, branch_name)? {
            let upstream_hash = git
                .repo
                .find_reference(&upstream.refname)?
                .peel_to_commit()?
                .id();
            if upstream_hash != local_hash {
                warn!("fetch upstream is different from local branch");
            }
        }
        if let Some(upstream) = get_push_upstream(&git.repo, &git.config, branch_name)? {
            let upstream_hash = git
                .repo
                .find_reference(&upstream.refname)?
                .peel_to_commit()?
                .id();
            if upstream_hash != local_hash {
                warn!("fetch upstream is different from local branch");
            }
        }

        for base in &bases {
            base_and_branch_to_compare.push((base, branch_name.to_string()));
        }
    }

    let remote_heads_per_url = remote_urls
        .into_par_iter()
        .map({
            let git = ForceSendSync(git);
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
            let git = ForceSendSync(git);
            move |(base, branch_name)| {
                classify(
                    git,
                    &merged_locals,
                    &remote_heads_per_url,
                    &base,
                    &branch_name,
                )
                .with_context(|| format!("base={:?}, branch_name={}", base, branch_name))
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
    result.keep_base(&git.repo, &git.config, &config.bases)?;
    result.keep_protected(&git.repo, &git.config, &config.protected_branches)?;
    result.keep_non_heads_remotes();
    result.apply_filter(&config.filter)?;

    if !config.detach {
        result.adjust_not_to_detach(&git.repo)?;
    }

    Ok(result)
}

#[derive(Debug, Clone)]
struct Ref {
    name: String,
    commit: String,
}

impl Ref {
    fn from_name(repo: &Repository, refname: &str) -> Result<Ref> {
        Ok(Ref {
            name: refname.to_string(),
            commit: repo
                .resolve_reference_from_short_name(refname)?
                .peel_to_commit()?
                .id()
                .to_string(),
        })
    }
}

#[derive(Debug, Clone)]
struct UpstreamMergeState {
    upstream: Ref,
    merged: bool,
}

struct Classification {
    branch: Ref,
    branch_is_merged: bool,
    fetch: Option<UpstreamMergeState>,
    push: Option<UpstreamMergeState>,
    messages: Vec<&'static str>,
    result: MergedOrStray,
}

impl Classification {
    fn merged_or_stray_remote(
        &mut self,
        repo: &Repository,
        merge_state: &UpstreamMergeState,
    ) -> Result<()> {
        if merge_state.merged {
            self.messages
                .push("fetch upstream is merged, but forget to delete");
            self.merged_remote(repo, &merge_state.upstream)
        } else {
            self.messages.push("fetch upstream is not merged");
            self.stray_remote(repo, &merge_state.upstream)
        }
    }

    fn merged_remote(&mut self, repo: &Repository, upstream: &Ref) -> Result<()> {
        self.result
            .merged_remotes
            .insert(RemoteTrackingBranch::new(&upstream.name).remote_branch(&repo)?);
        Ok(())
    }

    fn stray_remote(&mut self, repo: &Repository, upstream: &Ref) -> Result<()> {
        self.result
            .stray_remotes
            .insert(RemoteTrackingBranch::new(&upstream.name).remote_branch(&repo)?);
        Ok(())
    }
}

/// Make sure repo and config are semantically Send + Sync.
fn classify(
    git: ForceSendSync<&Git>,
    merged_locals: &HashSet<String>,
    remote_heads_per_url: &HashMap<String, HashSet<String>>,
    base: &RemoteTrackingBranch,
    branch_name: &str,
) -> Result<Classification> {
    let branch = Ref::from_name(&git.repo, branch_name)?;
    let branch_is_merged = merged_locals.contains(branch_name)
        || subprocess::is_merged(&git.repo, &base.refname, branch_name)?;
    let fetch = if let Some(fetch) = get_fetch_upstream(&git.repo, &git.config, branch_name)? {
        let upstream = Ref::from_name(&git.repo, &fetch.refname)?;
        let merged = (branch_is_merged && upstream.commit == branch.commit)
            || subprocess::is_merged(&git.repo, &base.refname, &upstream.name)?;
        Some(UpstreamMergeState { upstream, merged })
    } else {
        None
    };
    let push = if let Some(push) = get_push_upstream(&git.repo, &git.config, branch_name)? {
        let upstream = Ref::from_name(&git.repo, &push.refname)?;
        let merged = (branch_is_merged && upstream.commit == branch.commit)
            || fetch
                .as_ref()
                .map(|x| x.merged && upstream.commit == x.upstream.commit)
                == Some(true)
            || subprocess::is_merged(&git.repo, &base.refname, &upstream.name)?;
        Some(UpstreamMergeState { upstream, merged })
    } else {
        None
    };

    let mut c = Classification {
        branch,
        branch_is_merged,
        fetch: fetch.clone(),
        push: push.clone(),
        messages: vec![],
        result: MergedOrStray::default(),
    };

    match (fetch, push) {
        (Some(fetch), Some(push)) if branch_is_merged => {
            c.messages.push("local is merged");
            c.result.merged_locals.insert(branch_name.to_string());
            c.merged_or_stray_remote(&git.repo, &fetch)?;
            c.merged_or_stray_remote(&git.repo, &push)?;
        }
        (Some(fetch), Some(push)) => {
            if fetch.merged || push.merged {
                c.messages
                    .push("some upstreams merged, but the local strays");
                c.result.stray_locals.insert(branch_name.to_string());
                c.merged_or_stray_remote(&git.repo, &push)?;
                c.merged_or_stray_remote(&git.repo, &fetch)?;
            }
        }

        (Some(fetch), None) => {
            if branch_is_merged {
                c.messages.push("local is merged");
                c.result.merged_locals.insert(branch_name.to_string());
                c.merged_or_stray_remote(&git.repo, &fetch)?;
            } else if fetch.merged {
                c.messages
                    .push("fetch upstream is merged, but the local strays");
                c.result.stray_locals.insert(branch_name.to_string());
                c.merged_remote(&git.repo, &fetch.upstream)?;
            }
        }

        (None, Some(push)) => {
            if branch_is_merged {
                c.messages.push("local is merged");
                c.result.merged_locals.insert(branch_name.to_string());
                c.merged_or_stray_remote(&git.repo, &push)?;
            } else if push.merged {
                c.messages
                    .push("push upstream is merged, but the local strays");
                c.result.stray_locals.insert(branch_name.to_string());
                c.merged_remote(&git.repo, &push.upstream)?;
            }
        }

        (None, None) if branch_is_merged => {
            let remote = config::get_remote_raw(&git.config, branch_name)?
                .expect("should have it if it has an upstream");
            let merge = config::get_merge(&git.config, branch_name)?
                .expect("should have it if it has an upstream");
            if remote_heads_per_url.contains_key(&remote)
                && remote_heads_per_url[&remote].contains(&merge)
            {
                c.messages.push(
                    "merged local, merged remote: the branch is merged, but forgot to delete",
                );
                c.result.merged_locals.insert(branch_name.to_string());
                c.result.merged_remotes.insert(RemoteBranch {
                    remote,
                    refname: merge,
                });
            } else {
                c.messages
                    .push("merged local: the branch is merged, and deleted");
                c.result.merged_locals.insert(branch_name.to_string());
            }
        }
        (None, None) => {
            // `origin` or `git@github.com:someone/fork.git`
            let remote = config::get_remote_raw(&git.config, branch_name)?
                .expect("should have it if it has an upstream");
            let merge = config::get_merge(&git.config, branch_name)?
                .expect("should have it if it has an upstream");
            if remote_heads_per_url.contains_key(&remote)
                && remote_heads_per_url[&remote].contains(&merge)
            {
                c.messages.push("skip: the branch is alive");
            } else {
                c.messages
                    .push("the branch is not merged but the remote is gone somehow");
                c.result.stray_locals.insert(branch_name.to_string());
            }
        }
    }

    Ok(c)
}

/// Use with caution.
/// It makes wrapping type T to be Send + Sync.
/// Make sure T is semantically Send + Sync
#[derive(Copy, Clone)]
struct ForceSendSync<T>(T);

unsafe impl<T> Sync for ForceSendSync<T> {}
unsafe impl<T> Send for ForceSendSync<T> {}

impl<T> Deref for ForceSendSync<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
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
        match repo.find_branch(base, BranchType::Local) {
            Ok(branch) => {
                let refname = branch.get().name().context("non utf-8 base branch ref")?;
                result.insert((*base).to_string());
                result.insert((*refname).to_string());
            }
            Err(err) if err.code() == ErrorCode::NotFound => continue,
            Err(err) => return Err(err.into()),
        }

        if let Some(upstream) = get_fetch_upstream(repo, config, base)? {
            result.insert(upstream.refname);
        }

        if let Some(upstream) = get_push_upstream(repo, config, base)? {
            result.insert(upstream.refname);
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
        // find "master -> refs/remotes/origin/master"
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

        if base.starts_with("refs/remotes/") {
            result.push(RemoteTrackingBranch::new(base));
            continue;
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
                result.insert(branch_name.to_string());
                if let Some(upstream) = get_fetch_upstream(repo, config, branch_name)? {
                    result.insert(upstream.refname);
                }
                if let Some(upstream) = get_push_upstream(repo, config, branch_name)? {
                    result.insert(upstream.refname);
                }
                let reference = branch.into_reference();
                let refname = reference.name().context("non utf-8 ref")?;
                result.insert(refname.to_string());
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
        assert!(head_refname.starts_with("refs/heads/"));
        let head_name = &head_refname["refs/heads/".len()..];
        if branches.contains(&head_name) {
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
