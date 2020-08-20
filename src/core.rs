use std::collections::HashSet;
use std::convert::TryFrom;

use anyhow::{Context, Result};
use git2::{BranchType, Repository};
use log::*;
use rayon::prelude::*;

use crate::args::DeleteFilter;
use crate::branch::{LocalBranch, RemoteBranch, RemoteTrackingBranch};
use crate::merge_tracker::{MergeState, MergeTracker};
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

pub struct Classification {
    pub local: MergeState<LocalBranch>,
    pub fetch: Option<MergeState<RemoteTrackingBranch>>,
    pub messages: Vec<&'static str>,
    pub result: HashSet<ClassifiedBranch>,
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

/// Make sure repo and config are semantically Send + Sync.
pub fn classify(
    git: ForceSendSync<&Git>,
    merge_tracker: &MergeTracker,
    remote_heads: &[RemoteHead],
    base: &RemoteTrackingBranch,
    branch: &LocalBranch,
) -> Result<Classification> {
    let local = merge_tracker.check_and_track(&git.repo, &base.refname, branch)?;
    let fetch = if let Some(fetch) = branch.fetch_upstream(&git.repo, &git.config)? {
        Some(merge_tracker.check_and_track(&git.repo, &base.refname, &fetch)?)
    } else {
        None
    };

    let mut c = Classification {
        local: local.clone(),
        fetch: fetch.clone(),
        messages: vec![],
        result: HashSet::default(),
    };

    match fetch {
        Some(upstream) => {
            if local.merged {
                if upstream.merged {
                    c.messages.push("local & fetch upstream are merged");
                    c.result
                        .insert(ClassifiedBranch::MergedLocal(branch.clone()));
                    c.result
                        .insert(ClassifiedBranch::MergedRemoteTracking(upstream.branch));
                } else {
                    c.messages.push("local & fetch upstream are diverged");
                    c.result.insert(ClassifiedBranch::DivergedRemoteTracking {
                        local: branch.clone(),
                        upstream: upstream.branch,
                    });
                }
            } else if upstream.merged {
                c.messages.push("upstream is merged, but the local strays");
                c.result.insert(ClassifiedBranch::Stray(branch.clone()));
                c.result
                    .insert(ClassifiedBranch::MergedRemoteTracking(upstream.branch));
            }
        }

        // `hub-cli` sets config `branch.{branch_name}.remote` as URL without `remote.{remote}` entry.
        // `fetch_upstream` returns None.
        // However we can try manual classification without `remote.{remote}` entry.
        None => {
            let remote = config::get_remote_name(&git.config, branch)?
                .expect("should have it if it has an upstream");
            let merge = config::get_merge(&git.config, branch)?
                .expect("should have it if it has an upstream");
            let remote_head = remote_heads
                .iter()
                .find(|h| h.remote == remote && h.refname == merge)
                .map(|h| &h.commit);

            match (local.merged, remote_head) {
                (true, Some(head)) if head == &local.commit => {
                    c.messages.push(
                        "merged local, merged remote: the branch is merged, but forgot to delete",
                    );
                    c.result.insert(ClassifiedBranch::MergedDirectFetch {
                        local: branch.clone(),
                        remote: RemoteBranch {
                            remote,
                            refname: merge,
                        },
                    });
                }
                (true, Some(_)) => {
                    c.messages.push(
                        "merged local, diverged upstream: the branch is merged, but upstream is diverged",
                    );
                    c.result.insert(ClassifiedBranch::DivergedDirectFetch {
                        local: branch.clone(),
                        remote: RemoteBranch {
                            remote,
                            refname: merge,
                        },
                    });
                }
                (true, None) => {
                    c.messages
                        .push("merged local: the branch is merged, and deleted");
                    c.result
                        .insert(ClassifiedBranch::MergedLocal(branch.clone()));
                }
                (false, None) => {
                    c.messages
                        .push("the branch is not merged but the remote is gone somehow");
                    c.result.insert(ClassifiedBranch::Stray(branch.clone()));
                }
                (false, _) => {
                    c.messages.push("skip: the branch is alive");
                }
            }
        }
    }

    Ok(c)
}

pub fn get_tracking_branches(
    git: &Git,
    base_upstreams: &[RemoteTrackingBranch],
) -> Result<Vec<LocalBranch>> {
    let mut result = Vec::new();
    for branch in git.repo.branches(Some(BranchType::Local))? {
        let branch = LocalBranch::try_from(&branch?.0)?;

        if config::get_remote_name(&git.config, &branch)?.is_none() {
            continue;
        }

        let fetch_upstream = branch.fetch_upstream(&git.repo, &git.config)?;
        if let Some(upstream) = &fetch_upstream {
            if base_upstreams.contains(&upstream) {
                debug!("Skip: the branch tracks the base: {:?}", branch);
                continue;
            }
        }

        result.push(branch);
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
    for tracking_branch in tracking_branches {
        let upstream = tracking_branch.fetch_upstream(&git.repo, &git.config)?;
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

pub fn get_remote_heads(git: &Git, branches: &[LocalBranch]) -> Result<Vec<RemoteHead>> {
    let mut remote_urls = Vec::new();

    for branch in branches {
        if let Some(remote) = config::get_remote_name(&git.config, &branch)? {
            if config::get_remote(&git.repo, &remote)?.is_none() {
                remote_urls.push(remote);
            }
        }
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
