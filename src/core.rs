use std::collections::HashSet;
use std::convert::TryFrom;
use std::fmt::Debug;

use anyhow::{Context, Result};
use crossbeam_channel::unbounded;
use git2::{BranchType, Repository};
use log::*;
use rayon::prelude::*;

use crate::args::DeleteFilter;
use crate::branch::{
    LocalBranch, Refname, RemoteBranch, RemoteTrackingBranch, RemoteTrackingBranchStatus,
};
use crate::merge_tracker::MergeTracker;
use crate::subprocess::{self, get_worktrees, RemoteHead};
use crate::util::ForceSendSync;
use crate::{config, Git};

#[derive(Default)]
pub struct TrimPlan {
    pub to_delete: HashSet<ClassifiedBranch>,
    pub preserved: Vec<Preserved>,
}

pub struct Preserved {
    pub branch: ClassifiedBranch,
    pub reason: String,
}

impl TrimPlan {
    pub fn locals_to_delete(&self) -> Vec<&LocalBranch> {
        let mut result = Vec::new();
        for branch in &self.to_delete {
            if let Some(local) = branch.local() {
                result.push(local)
            }
        }
        result
    }

    pub fn remotes_to_delete(&self, repo: &Repository) -> Result<Vec<RemoteBranch>> {
        let mut result = Vec::new();
        for branch in &self.to_delete {
            if let Some(remote) = branch.remote(repo)? {
                result.push(remote);
            }
        }
        Ok(result)
    }
}

impl TrimPlan {
    pub fn preserve(
        &mut self,
        preserved_refnames: &HashSet<String>,
        reason: &'static str,
    ) -> Result<()> {
        let mut preserve = Vec::new();
        for branch in &self.to_delete {
            let contained = match &branch {
                ClassifiedBranch::MergedLocal(local)
                | ClassifiedBranch::Stray(local)
                | ClassifiedBranch::MergedDirectFetch { local, .. }
                | ClassifiedBranch::DivergedDirectFetch { local, .. }
                | ClassifiedBranch::MergedNonTrackingLocal(local) => {
                    preserved_refnames.contains(&local.refname)
                }
                ClassifiedBranch::MergedRemoteTracking(upstream)
                | ClassifiedBranch::MergedNonUpstreamRemoteTracking(upstream) => {
                    preserved_refnames.contains(&upstream.refname)
                }
                ClassifiedBranch::DivergedRemoteTracking { local, upstream } => {
                    let preserve_local = preserved_refnames.contains(&local.refname);
                    let preserve_remote = preserved_refnames.contains(&upstream.refname);
                    preserve_local || preserve_remote
                }
            };

            if !contained {
                continue;
            }

            preserve.push(Preserved {
                branch: branch.clone(),
                reason: reason.to_owned(),
            });
        }

        for preserved in &preserve {
            self.to_delete.remove(&preserved.branch);
        }
        self.preserved.extend(preserve);

        Ok(())
    }

    pub fn preserve_protected(
        &mut self,
        repo: &Repository,
        preserved_patterns: &[&str],
    ) -> Result<()> {
        let mut preserve = Vec::new();
        for branch in &self.to_delete {
            let pattern =
                match &branch {
                    ClassifiedBranch::MergedLocal(local)
                    | ClassifiedBranch::Stray(local)
                    | ClassifiedBranch::MergedDirectFetch { local, .. }
                    | ClassifiedBranch::DivergedDirectFetch { local, .. }
                    | ClassifiedBranch::MergedNonTrackingLocal(local) => {
                        get_protect_pattern(&repo, preserved_patterns, local)?
                    }
                    ClassifiedBranch::MergedRemoteTracking(upstream)
                    | ClassifiedBranch::MergedNonUpstreamRemoteTracking(upstream) => {
                        get_protect_pattern(&repo, preserved_patterns, upstream)?
                    }
                    ClassifiedBranch::DivergedRemoteTracking { local, upstream } => {
                        get_protect_pattern(&repo, preserved_patterns, local)?
                            .or(get_protect_pattern(&repo, preserved_patterns, upstream)?)
                    }
                };

            if let Some(pattern) = pattern {
                preserve.push(Preserved {
                    branch: branch.clone(),
                    reason: format!("protected by a pattern `{}`", pattern),
                });
            }
        }

        for preserved in &preserve {
            self.to_delete.remove(&preserved.branch);
        }
        self.preserved.extend(preserve);

        Ok(())
    }

    /// `hub-cli` can checkout pull request branch. However they are stored in `refs/pulls/`.
    /// This prevents to remove them.
    pub fn preserve_non_heads_remotes(&mut self, repo: &Repository) -> Result<()> {
        let mut preserve = Vec::new();

        for branch in &self.to_delete {
            let remote = if let Some(remote) = branch.remote(repo)? {
                remote
            } else {
                continue;
            };

            if !remote.refname.starts_with("refs/heads/") {
                trace!("filter-out: remote ref {}", remote);
                preserve.push(Preserved {
                    branch: branch.clone(),
                    reason: "a non-heads remote".to_owned(),
                });
            }
        }

        for preserved in &preserve {
            self.to_delete.remove(&preserved.branch);
        }
        self.preserved.extend(preserve);

        Ok(())
    }

    pub fn preserve_worktree(&mut self, repo: &Repository) -> Result<()> {
        let worktrees = get_worktrees(repo)?;
        let mut preserve = Vec::new();
        for branch in &self.to_delete {
            let local = if let Some(local) = branch.local() {
                local
            } else {
                continue;
            };
            if let Some(path) = worktrees.get(local) {
                preserve.push(Preserved {
                    branch: branch.clone(),
                    reason: format!("worktree at {}", path),
                });
            }
        }

        for preserved in &preserve {
            self.to_delete.remove(&preserved.branch);
        }
        self.preserved.extend(preserve);

        Ok(())
    }

    pub fn apply_delete_filter(&mut self, repo: &Repository, filter: &DeleteFilter) -> Result<()> {
        let mut preserve = Vec::new();

        for branch in &self.to_delete {
            let delete = match branch {
                ClassifiedBranch::MergedLocal(_) => filter.delete_merged_local(),
                ClassifiedBranch::Stray(_) => filter.delete_stray(),
                ClassifiedBranch::MergedRemoteTracking(upstream) => {
                    let remote = upstream.to_remote_branch(repo)?;
                    filter.delete_merged_remote(&remote.remote)
                }
                ClassifiedBranch::DivergedRemoteTracking { upstream, .. } => {
                    let remote = upstream.to_remote_branch(repo)?;
                    filter.delete_diverged(&remote.remote)
                }

                ClassifiedBranch::MergedDirectFetch { remote, .. } => {
                    filter.delete_merged_remote(&remote.remote)
                }
                ClassifiedBranch::DivergedDirectFetch { remote, .. } => {
                    filter.delete_diverged(&remote.remote)
                }

                ClassifiedBranch::MergedNonTrackingLocal(_) => {
                    filter.delete_merged_non_tracking_local()
                }
                ClassifiedBranch::MergedNonUpstreamRemoteTracking(upstream) => {
                    let remote = upstream.to_remote_branch(repo)?;
                    filter.delete_merged_non_upstream_remote_tracking(&remote.remote)
                }
            };

            trace!("Delete filter result: {:?} => {}", branch, delete);

            if !delete {
                preserve.push(Preserved {
                    branch: branch.clone(),
                    reason: "filtered".to_owned(),
                });
            }
        }

        for preserved in &preserve {
            self.to_delete.remove(&preserved.branch);
        }
        self.preserved.extend(preserve);

        Ok(())
    }

    pub fn adjust_not_to_detach(&mut self, repo: &Repository) -> Result<()> {
        if repo.head_detached()? {
            return Ok(());
        }
        let head = repo.head()?;
        let head_name = head.name().context("non-utf8 head ref name")?;
        let head_branch = LocalBranch::new(head_name);

        let mut preserve = Vec::new();

        for branch in &self.to_delete {
            if branch.local() == Some(&head_branch) {
                preserve.push(Preserved {
                    branch: branch.clone(),
                    reason: "HEAD".to_owned(),
                });
            }
        }

        for preserved in &preserve {
            self.to_delete.remove(&preserved.branch);
        }
        self.preserved.extend(preserve);
        Ok(())
    }

    pub fn get_preserved_local(&self, target: &LocalBranch) -> Option<&Preserved> {
        for preserved in &self.preserved {
            if preserved.branch.local() == Some(target) {
                return Some(preserved);
            }
        }
        None
    }

    pub fn get_preserved_upstream(&self, target: &RemoteTrackingBranch) -> Option<&Preserved> {
        for preserved in &self.preserved {
            if preserved.branch.upstream() == Some(target) {
                return Some(preserved);
            }
        }
        None
    }
}

fn get_protect_pattern<'a, B: Refname>(
    repo: &Repository,
    protected_patterns: &[&'a str],
    branch: &B,
) -> Result<Option<&'a str>> {
    let prefixes = &["", "refs/remotes/", "refs/heads/"];
    let target_refname = branch.refname();
    for protected_pattern in protected_patterns {
        for prefix in prefixes {
            for reference in repo.references_glob(&format!("{}{}", prefix, protected_pattern))? {
                let reference = reference?;
                let refname = reference.name().context("non utf-8 refname")?;
                if target_refname == refname {
                    return Ok(Some(protected_pattern));
                }
            }
        }
    }
    Ok(None)
}

#[derive(Hash, Eq, PartialEq, Debug, Clone)]
pub enum ClassifiedBranch {
    MergedLocal(LocalBranch),
    Stray(LocalBranch),
    MergedRemoteTracking(RemoteTrackingBranch),
    DivergedRemoteTracking {
        local: LocalBranch,
        upstream: RemoteTrackingBranch,
    },

    MergedDirectFetch {
        local: LocalBranch,
        remote: RemoteBranch,
    },
    DivergedDirectFetch {
        local: LocalBranch,
        remote: RemoteBranch,
    },

    MergedNonTrackingLocal(LocalBranch),
    MergedNonUpstreamRemoteTracking(RemoteTrackingBranch),
}

impl ClassifiedBranch {
    pub fn local(&self) -> Option<&LocalBranch> {
        match self {
            ClassifiedBranch::MergedLocal(local)
            | ClassifiedBranch::Stray(local)
            | ClassifiedBranch::DivergedRemoteTracking { local, .. }
            | ClassifiedBranch::MergedDirectFetch { local, .. }
            | ClassifiedBranch::DivergedDirectFetch { local, .. }
            | ClassifiedBranch::MergedNonTrackingLocal(local) => Some(local),
            _ => None,
        }
    }

    pub fn upstream(&self) -> Option<&RemoteTrackingBranch> {
        match self {
            ClassifiedBranch::MergedRemoteTracking(upstream)
            | ClassifiedBranch::DivergedRemoteTracking { upstream, .. }
            | ClassifiedBranch::MergedNonUpstreamRemoteTracking(upstream) => Some(upstream),
            _ => None,
        }
    }

    pub fn remote(&self, repo: &Repository) -> Result<Option<RemoteBranch>> {
        match self {
            ClassifiedBranch::MergedRemoteTracking(upstream)
            | ClassifiedBranch::DivergedRemoteTracking { upstream, .. }
            | ClassifiedBranch::MergedNonUpstreamRemoteTracking(upstream) => {
                let remote = upstream.to_remote_branch(repo)?;
                Ok(Some(remote))
            }
            ClassifiedBranch::MergedDirectFetch { remote, .. }
            | ClassifiedBranch::DivergedDirectFetch { remote, .. } => Ok(Some(remote.clone())),
            _ => Ok(None),
        }
    }

    pub fn message_local(&self) -> String {
        match self {
            ClassifiedBranch::MergedLocal(_) | ClassifiedBranch::MergedDirectFetch { .. } => {
                "merged".to_owned()
            }
            ClassifiedBranch::MergedNonTrackingLocal(_) => "merged non-tracking".to_owned(),
            ClassifiedBranch::Stray(_) => "stray".to_owned(),
            ClassifiedBranch::DivergedRemoteTracking {
                upstream: remote, ..
            } => format!("diverged with {}", remote.refname),
            ClassifiedBranch::DivergedDirectFetch { remote, .. } => {
                format!("diverged with {}", remote)
            }
            _ => "If you see this message, report this as a bug".to_owned(),
        }
    }

    pub fn message_remote(&self) -> String {
        match self {
            ClassifiedBranch::MergedRemoteTracking(_)
            | ClassifiedBranch::MergedDirectFetch { .. } => "merged".to_owned(),
            ClassifiedBranch::MergedNonUpstreamRemoteTracking(_) => {
                "merged non-upstream".to_owned()
            }
            ClassifiedBranch::DivergedRemoteTracking { local, .. } => {
                format!("diverged with {}", local.refname)
            }
            ClassifiedBranch::DivergedDirectFetch { local, .. } => {
                format!("diverged with {}", local.short_name())
            }
            _ => "If you see this message, report this as a bug".to_owned(),
        }
    }
}

pub struct Classifier<'a> {
    git: &'a Git,
    merge_tracker: &'a MergeTracker,
    tasks: Vec<Box<dyn FnOnce() -> Result<ClassificationResponseWithId> + Send + Sync + 'a>>,
}

impl<'a> Classifier<'a> {
    pub fn new(git: &'a Git, merge_tracker: &'a MergeTracker) -> Self {
        Self {
            git,
            merge_tracker,
            tasks: Vec::new(),
        }
    }

    pub fn queue_request<R: ClassificationRequest + Send + Sync + Debug + 'a>(&mut self, req: R) {
        let id = self.tasks.len();
        trace!("Enqueue #{}: {:#?}", id, req);
        let git = ForceSendSync::new(self.git);
        let merge_tracker = self.merge_tracker;
        self.tasks.push(Box::new(move || {
            req.classify(git, merge_tracker)
                .with_context(|| format!("Failed to classify #{}: {:#?}", id, req))
                .map(|response| ClassificationResponseWithId { id, response })
        }));
    }

    pub fn queue_request_with_context<
        R: ClassificationRequestWithContext<C> + Send + Sync + Debug + 'a,
        C: Send + Sync + 'a,
    >(
        &mut self,
        req: R,
        context: C,
    ) {
        let id = self.tasks.len();
        trace!("Enqueue #{}: {:#?}", id, req);
        let git = ForceSendSync::new(self.git);
        let merge_tracker = self.merge_tracker;
        self.tasks.push(Box::new(move || {
            req.classify_with_context(git, merge_tracker, context)
                .with_context(|| format!("Failed to classify #{}: {:#?}", id, req))
                .map(|response| ClassificationResponseWithId { id, response })
        }));
    }

    pub fn classify(self) -> Result<Vec<ClassificationResponse>> {
        info!("Classify {} requests", self.tasks.len());
        let tasks = self.tasks;
        let receiver = rayon::scope(move |scope| {
            let (sender, receiver) = unbounded();
            for tasks in tasks {
                let sender = sender.clone();
                scope.spawn(move |_| {
                    let result = tasks();
                    sender.send(result).unwrap();
                })
            }
            receiver
        });

        let mut results = Vec::new();
        for result in receiver {
            let ClassificationResponseWithId { id, response } = result?;
            debug!("Result #{}: {:#?}", id, response);

            results.push(response);
        }

        Ok(results)
    }
}

struct ClassificationResponseWithId {
    id: usize,
    response: ClassificationResponse,
}

#[derive(Debug)]
pub struct ClassificationResponse {
    message: &'static str,
    pub result: Vec<ClassifiedBranch>,
}

pub trait ClassificationRequest {
    fn classify(
        &self,
        git: ForceSendSync<&Git>,
        merge_tracker: &MergeTracker,
    ) -> Result<ClassificationResponse>;
}

pub trait ClassificationRequestWithContext<C> {
    fn classify_with_context(
        &self,
        git: ForceSendSync<&Git>,
        merge_tracker: &MergeTracker,
        context: C,
    ) -> Result<ClassificationResponse>;
}

#[derive(Debug)]
pub struct TrackingBranchClassificationRequest<'a> {
    pub base: &'a RemoteTrackingBranch,
    pub local: &'a LocalBranch,
    pub upstream: Option<&'a RemoteTrackingBranch>,
}

impl<'a> ClassificationRequest for TrackingBranchClassificationRequest<'a> {
    fn classify(
        &self,
        git: ForceSendSync<&Git>,
        merge_tracker: &MergeTracker,
    ) -> Result<ClassificationResponse> {
        let local = merge_tracker.check_and_track(&git.repo, &self.base.refname, self.local)?;
        let upstream = if let Some(upstream) = self.upstream {
            merge_tracker.check_and_track(&git.repo, &self.base.refname, upstream)?
        } else {
            let result = if local.merged {
                ClassificationResponse {
                    message: "local is merged but remote is gone",
                    result: vec![ClassifiedBranch::MergedLocal(local.branch)],
                }
            } else {
                ClassificationResponse {
                    message: "local is stray but remote is gone",
                    result: vec![ClassifiedBranch::Stray(local.branch)],
                }
            };
            return Ok(result);
        };

        let result = match (local.merged, upstream.merged) {
            (true, true) => ClassificationResponse {
                message: "local & upstream are merged",
                result: vec![
                    ClassifiedBranch::MergedLocal(local.branch),
                    ClassifiedBranch::MergedRemoteTracking(upstream.branch),
                ],
            },
            (true, false) => ClassificationResponse {
                message: "local is merged but diverged with upstream",
                result: vec![ClassifiedBranch::DivergedRemoteTracking {
                    local: local.branch,
                    upstream: upstream.branch,
                }],
            },
            (false, true) => ClassificationResponse {
                message: "upstream is merged, but the local strays",
                result: vec![
                    ClassifiedBranch::Stray(local.branch),
                    ClassifiedBranch::MergedRemoteTracking(upstream.branch),
                ],
            },
            (false, false) => ClassificationResponse {
                message: "local & upstream are not merged yet",
                result: vec![],
            },
        };

        Ok(result)
    }
}

/// `hub-cli` style branch classification request.
/// `hub-cli` sets config `branch.{branch_name}.remote` as URL without `remote.{remote}` entry.
/// However we can try manual classification without `remote.{remote}` entry.
#[derive(Debug)]
pub struct DirectFetchClassificationRequest<'a> {
    pub base: &'a RemoteTrackingBranch,
    pub local: &'a LocalBranch,
    pub remote: &'a RemoteBranch,
}

impl<'a> ClassificationRequestWithContext<&'a [RemoteHead]>
    for DirectFetchClassificationRequest<'a>
{
    fn classify_with_context(
        &self,
        git: ForceSendSync<&Git>,
        merge_tracker: &MergeTracker,
        remote_heads: &[RemoteHead],
    ) -> Result<ClassificationResponse> {
        let local = merge_tracker.check_and_track(&git.repo, &self.base.refname, self.local)?;
        let remote_head = remote_heads
            .iter()
            .find(|h| h.remote == self.remote.remote && h.refname == self.remote.refname)
            .map(|h| &h.commit);

        let result = match (local.merged, remote_head) {
            (true, Some(head)) if head == &local.commit => ClassificationResponse {
                message: "local & remote are merged",
                result: vec![ClassifiedBranch::MergedDirectFetch {
                    local: local.branch,
                    remote: self.remote.clone(),
                }],
            },
            (true, Some(_)) => ClassificationResponse {
                message: "local is merged, but diverged with upstream",
                result: vec![ClassifiedBranch::DivergedDirectFetch {
                    local: local.branch,
                    remote: self.remote.clone(),
                }],
            },
            (true, None) => ClassificationResponse {
                message: "local is merged and its upstream is gone",
                result: vec![ClassifiedBranch::MergedLocal(local.branch)],
            },
            (false, None) => ClassificationResponse {
                message: "local is not merged but the remote is gone somehow",
                result: vec![ClassifiedBranch::Stray(local.branch)],
            },
            (false, _) => ClassificationResponse {
                message: "local is not merged yet",
                result: vec![],
            },
        };

        Ok(result)
    }
}

#[derive(Debug)]
pub struct NonTrackingBranchClassificationRequest<'a> {
    pub base: &'a RemoteTrackingBranch,
    pub local: &'a LocalBranch,
}

impl<'a> ClassificationRequest for NonTrackingBranchClassificationRequest<'a> {
    fn classify(
        &self,
        git: ForceSendSync<&Git>,
        merge_tracker: &MergeTracker,
    ) -> Result<ClassificationResponse> {
        let local = merge_tracker.check_and_track(&git.repo, &self.base.refname, self.local)?;
        let result = if local.merged {
            ClassificationResponse {
                message: "non-tracking local is merged",
                result: vec![ClassifiedBranch::MergedNonTrackingLocal(local.branch)],
            }
        } else {
            ClassificationResponse {
                message: "non-tracking local is not merged",
                result: vec![],
            }
        };
        Ok(result)
    }
}

#[derive(Debug)]
pub struct NonUpstreamBranchClassificationRequest<'a> {
    pub base: &'a RemoteTrackingBranch,
    pub remote: &'a RemoteTrackingBranch,
}

impl<'a> ClassificationRequest for NonUpstreamBranchClassificationRequest<'a> {
    fn classify(
        &self,
        git: ForceSendSync<&Git>,
        merge_tracker: &MergeTracker,
    ) -> Result<ClassificationResponse> {
        let remote = merge_tracker.check_and_track(&git.repo, &self.base.refname, self.remote)?;
        let result = if remote.merged {
            ClassificationResponse {
                message: "non-upstream local is merged",
                result: vec![ClassifiedBranch::MergedNonUpstreamRemoteTracking(
                    remote.branch,
                )],
            }
        } else {
            ClassificationResponse {
                message: "non-upstream local is not merged",
                result: vec![],
            }
        };
        Ok(result)
    }
}

pub fn get_tracking_branches(
    git: &Git,
    base_upstreams: &[RemoteTrackingBranch],
) -> Result<Vec<(LocalBranch, Option<RemoteTrackingBranch>)>> {
    let mut result = Vec::new();
    for branch in git.repo.branches(Some(BranchType::Local))? {
        let local = LocalBranch::try_from(&branch?.0)?;

        match local.fetch_upstream(&git.repo, &git.config)? {
            RemoteTrackingBranchStatus::Exists(upstream) => {
                if base_upstreams.contains(&upstream) {
                    continue;
                }
                result.push((local, Some(upstream)));
            }
            RemoteTrackingBranchStatus::Gone(_) => result.push((local, None)),
            _ => {
                continue;
            }
        };
    }

    Ok(result)
}

/// Get `hub-cli` style direct fetched branches
pub fn get_direct_fetch_branches(
    git: &Git,
    base_refs: &[String],
) -> Result<Vec<(LocalBranch, RemoteBranch)>> {
    let mut result = Vec::new();
    for branch in git.repo.branches(Some(BranchType::Local))? {
        let local = LocalBranch::try_from(&branch?.0)?;

        if base_refs.contains(&local.refname) {
            continue;
        }

        let remote = if let Some(remote) = config::get_remote_name(&git.config, &local)? {
            remote
        } else {
            continue;
        };

        if config::get_remote(&git.repo, &remote)?.is_some() {
            continue;
        }

        let merge = config::get_merge(&git.config, &local)?.context(format!(
            "Should have `branch.{}.merge` entry on git config",
            local.short_name()
        ))?;

        let remote = RemoteBranch {
            remote,
            refname: merge,
        };

        result.push((local, remote));
    }

    Ok(result)
}

/// Get local branches that doesn't track any branch.
pub fn get_non_tracking_local_branches(
    git: &Git,
    base_refs: &[String],
) -> Result<Vec<LocalBranch>> {
    let mut result = Vec::new();
    for branch in git.repo.branches(Some(BranchType::Local))? {
        let branch = LocalBranch::try_from(&branch?.0)?;

        if config::get_remote_name(&git.config, &branch)?.is_some() {
            continue;
        }

        if base_refs.contains(&branch.refname) {
            continue;
        }

        result.push(branch);
    }

    Ok(result)
}

/// Get remote tracking branches that doesn't tracked by any branch.
pub fn get_non_upstream_remote_tracking_branches(
    git: &Git,
    base_upstreams: &[RemoteTrackingBranch],
) -> Result<Vec<RemoteTrackingBranch>> {
    let mut upstreams = HashSet::new();

    for base_upstream in base_upstreams {
        upstreams.insert(base_upstream.clone());
    }

    let tracking_branches = get_tracking_branches(git, base_upstreams)?;
    for (_local, upstream) in tracking_branches {
        if let Some(upstream) = upstream {
            upstreams.insert(upstream);
        }
    }

    let mut result = Vec::new();
    for branch in git.repo.branches(Some(BranchType::Remote))? {
        let (branch, _) = branch?;
        if branch.get().symbolic_target_bytes().is_some() {
            continue;
        }

        let branch = RemoteTrackingBranch::try_from(&branch)?;

        if upstreams.contains(&branch) {
            continue;
        }

        result.push(branch);
    }

    Ok(result)
}

pub fn get_remote_heads(git: &Git, branches: &[RemoteBranch]) -> Result<Vec<RemoteHead>> {
    let mut remote_urls = Vec::new();

    for branch in branches {
        remote_urls.push(&branch.remote);
    }

    Ok(remote_urls
        .into_par_iter()
        .map({
            let git = ForceSendSync::new(git);
            move |remote_url| {
                subprocess::ls_remote_heads(&git.repo, &remote_url)
                    .with_context(|| format!("remote_url={}", remote_url))
            }
        })
        .collect::<Result<Vec<Vec<RemoteHead>>, _>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<RemoteHead>>())
}
