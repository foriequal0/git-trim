use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use git2::{Oid, Repository, Signature};
use log::*;

use crate::args::DeleteFilter;
use crate::branch::{
    get_fetch_upstream, get_push_upstream, LocalBranch, RemoteBranch, RemoteTrackingBranch,
};
use crate::subprocess::is_merged_by_rev_list;
use crate::util::ForceSendSync;
use crate::{config, Git};

#[derive(Default, Eq, PartialEq, Debug)]
pub struct MergedOrStray {
    pub merged_locals: HashSet<LocalBranch>,
    pub stray_locals: HashSet<LocalBranch>,

    pub merged_remotes: HashSet<RemoteBranch>,
    pub stray_remotes: HashSet<RemoteBranch>,
}

impl MergedOrStray {
    pub fn accumulate(mut self, mut other: Self) -> Self {
        self.merged_locals.extend(other.merged_locals.drain());
        self.stray_locals.extend(other.stray_locals.drain());
        self.merged_remotes.extend(other.merged_remotes.drain());
        self.stray_remotes.extend(other.stray_remotes.drain());

        self
    }

    pub fn locals(&self) -> Vec<&LocalBranch> {
        self.merged_locals
            .iter()
            .chain(self.stray_locals.iter())
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
    pub kept_backs: HashMap<LocalBranch, Reason>,
    pub kept_back_remotes: HashMap<RemoteBranch, Reason>,
}

#[derive(Clone, Eq, PartialEq, Debug, Ord, PartialOrd)]
pub struct Reason {
    pub original_classification: OriginalClassification,
    pub message: &'static str,
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
    pub fn keep_base(&mut self, repo: &Repository, base_refs: &HashSet<String>) -> Result<()> {
        trace!("base_refs: {:#?}", base_refs);
        self.kept_backs.extend(keep_branches(
            &base_refs,
            Reason {
                original_classification: OriginalClassification::MergedLocal,
                message: "a base branch",
            },
            &mut self.to_delete.merged_locals,
        )?);
        self.kept_backs.extend(keep_branches(
            &base_refs,
            Reason {
                original_classification: OriginalClassification::StrayLocal,
                message: "a base branch",
            },
            &mut self.to_delete.stray_locals,
        )?);
        self.kept_back_remotes.extend(keep_remote_branches(
            repo,
            &base_refs,
            Reason {
                original_classification: OriginalClassification::MergedRemote,
                message: "a base branch",
            },
            &mut self.to_delete.merged_remotes,
        )?);
        self.kept_back_remotes.extend(keep_remote_branches(
            repo,
            &base_refs,
            Reason {
                original_classification: OriginalClassification::StrayRemote,
                message: "a base branch",
            },
            &mut self.to_delete.stray_remotes,
        )?);
        Ok(())
    }

    pub fn keep_protected(
        &mut self,
        repo: &Repository,
        protected_refs: &HashSet<String>,
    ) -> Result<()> {
        trace!("protected_refs: {:#?}", protected_refs);
        self.kept_backs.extend(keep_branches(
            &protected_refs,
            Reason {
                original_classification: OriginalClassification::MergedLocal,
                message: "a protected branch",
            },
            &mut self.to_delete.merged_locals,
        )?);
        self.kept_backs.extend(keep_branches(
            &protected_refs,
            Reason {
                original_classification: OriginalClassification::StrayLocal,
                message: "a protected branch",
            },
            &mut self.to_delete.stray_locals,
        )?);
        self.kept_back_remotes.extend(keep_remote_branches(
            repo,
            &protected_refs,
            Reason {
                original_classification: OriginalClassification::MergedRemote,
                message: "a protected branch",
            },
            &mut self.to_delete.merged_remotes,
        )?);
        self.kept_back_remotes.extend(keep_remote_branches(
            repo,
            &protected_refs,
            Reason {
                original_classification: OriginalClassification::StrayRemote,
                message: "a protected branch",
            },
            &mut self.to_delete.stray_remotes,
        )?);
        Ok(())
    }

    /// `hub-cli` can checkout pull request branch. However they are stored in `refs/pulls/`.
    /// This prevents to remove them.
    pub fn keep_non_heads_remotes(&mut self) {
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
                        message: "a non-heads remote branch",
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
                        message: "a non-heads remote branch",
                    },
                );
            }
        }
        self.to_delete.stray_remotes = stray_remotes;
    }

    pub fn apply_filter(&mut self, filter: &DeleteFilter) -> Result<()> {
        trace!("Before filter: {:#?}", self);
        trace!("Applying filter: {:?}", filter);
        if !filter.filter_merged_local() {
            trace!(
                "filter-out: merged local branches {:?}",
                self.to_delete.merged_locals
            );
            self.kept_backs
                .extend(self.to_delete.merged_locals.drain().map(|refname| {
                    (
                        refname,
                        Reason {
                            original_classification: OriginalClassification::MergedLocal,
                            message: "out of filter scope",
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
                .extend(self.to_delete.stray_locals.drain().map(|refname| {
                    (
                        refname,
                        Reason {
                            original_classification: OriginalClassification::StrayLocal,
                            message: "out of filter scope",
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
                        message: "out of filter scope",
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
                        message: "out of filter scope",
                    },
                );
            }
        }
        self.to_delete.stray_remotes = stray_remotes;

        Ok(())
    }

    pub fn adjust_not_to_detach(&mut self, repo: &Repository) -> Result<()> {
        if repo.head_detached()? {
            return Ok(());
        }
        let head = repo.head()?;
        let head_name = head.name().context("non-utf8 head ref name")?;
        let head_branch = LocalBranch::new(head_name);

        if self.to_delete.merged_locals.contains(&head_branch) {
            self.to_delete.merged_locals.remove(&head_branch);
            self.kept_backs.insert(
                head_branch.clone(),
                Reason {
                    original_classification: OriginalClassification::MergedLocal,
                    message: "not to make detached HEAD",
                },
            );
        }
        if self.to_delete.stray_locals.contains(&head_branch) {
            self.to_delete.stray_locals.remove(&head_branch);
            self.kept_backs.insert(
                head_branch,
                Reason {
                    original_classification: OriginalClassification::StrayLocal,
                    message: "not to make detached HEAD",
                },
            );
        }
        Ok(())
    }
}

fn keep_branches(
    protected_refs: &HashSet<String>,
    reason: Reason,
    branches: &mut HashSet<LocalBranch>,
) -> Result<HashMap<LocalBranch, Reason>> {
    let mut kept_back = HashMap::new();
    let mut bag = HashSet::new();
    for branch in branches.iter() {
        if protected_refs.contains(&branch.refname) {
            bag.insert(branch.clone());
            kept_back.insert(branch.clone(), reason.clone());
        }
    }
    for branch in bag.into_iter() {
        branches.remove(&branch);
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

#[derive(Debug, Clone)]
pub struct Ref {
    name: String,
    commit: String,
}

impl Ref {
    fn from_name(repo: &Repository, refname: &str) -> Result<Ref> {
        Ok(Ref {
            name: refname.to_string(),
            commit: repo
                .find_reference(refname)?
                .peel_to_commit()?
                .id()
                .to_string(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct UpstreamMergeState {
    upstream: Ref,
    merged: bool,
}

pub struct Classification {
    pub branch: Ref,
    pub branch_is_merged: bool,
    pub fetch: Option<UpstreamMergeState>,
    pub push: Option<UpstreamMergeState>,
    pub messages: Vec<&'static str>,
    pub result: MergedOrStray,
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
pub fn classify(
    git: ForceSendSync<&Git>,
    merge_tracker: &MergeTracker,
    remote_heads_per_url: &HashMap<String, HashSet<String>>,
    base: &RemoteTrackingBranch,
    branch: &LocalBranch,
) -> Result<Classification> {
    let branch_ref = Ref::from_name(&git.repo, &branch.refname)?;
    let branch_is_merged =
        merge_tracker.check_and_track(&git.repo, &base.refname, &branch.refname)?;
    let fetch = if let Some(fetch) = get_fetch_upstream(&git.repo, &git.config, branch)? {
        let upstream = Ref::from_name(&git.repo, &fetch.refname)?;
        let merged = merge_tracker.check_and_track(&git.repo, &base.refname, &upstream.name)?;
        Some(UpstreamMergeState { upstream, merged })
    } else {
        None
    };
    let push = if let Some(push) = get_push_upstream(&git.repo, &git.config, branch)? {
        let upstream = Ref::from_name(&git.repo, &push.refname)?;
        let merged = merge_tracker.check_and_track(&git.repo, &base.refname, &upstream.name)?;
        Some(UpstreamMergeState { upstream, merged })
    } else {
        None
    };

    let mut c = Classification {
        branch: branch_ref,
        branch_is_merged,
        fetch: fetch.clone(),
        push: push.clone(),
        messages: vec![],
        result: MergedOrStray::default(),
    };

    match (fetch, push) {
        (Some(fetch), Some(push)) => {
            if branch_is_merged {
                c.messages.push("local is merged");
                c.result.merged_locals.insert(branch.clone());
                c.merged_or_stray_remote(&git.repo, &fetch)?;
                c.merged_or_stray_remote(&git.repo, &push)?;
            } else if fetch.merged || push.merged {
                c.messages
                    .push("some upstreams are merged, but the local strays");
                c.result.stray_locals.insert(branch.clone());
                c.merged_or_stray_remote(&git.repo, &push)?;
                c.merged_or_stray_remote(&git.repo, &fetch)?;
            }
        }

        (Some(upstream), None) | (None, Some(upstream)) => {
            if branch_is_merged {
                c.messages.push("local is merged");
                c.result.merged_locals.insert(branch.clone());
                c.merged_or_stray_remote(&git.repo, &upstream)?;
            } else if upstream.merged {
                c.messages.push("upstream is merged, but the local strays");
                c.result.stray_locals.insert(branch.clone());
                c.merged_remote(&git.repo, &upstream.upstream)?;
            }
        }

        // `hub-cli` sets config `branch.{branch_name}.remote` as URL without `remote.{remote}` entry.
        // so `get_push_upstream` and `get_fetch_upstream` returns None.
        // However we can try manual classification without `remote.{remote}` entry.
        (None, None) => {
            let remote = config::get_remote_raw(&git.config, branch)?
                .expect("should have it if it has an upstream");
            let merge = config::get_merge(&git.config, branch)?
                .expect("should have it if it has an upstream");
            let upstream_is_exists = remote_heads_per_url.contains_key(&remote)
                && remote_heads_per_url[&remote].contains(&merge);

            if upstream_is_exists && branch_is_merged {
                c.messages.push(
                    "merged local, merged remote: the branch is merged, but forgot to delete",
                );
                c.result.merged_locals.insert(branch.clone());
                c.result.merged_remotes.insert(RemoteBranch {
                    remote,
                    refname: merge,
                });
            } else if branch_is_merged {
                c.messages
                    .push("merged local: the branch is merged, and deleted");
                c.result.merged_locals.insert(branch.clone());
            } else if !upstream_is_exists {
                c.messages
                    .push("the branch is not merged but the remote is gone somehow");
                c.result.stray_locals.insert(branch.clone());
            } else {
                c.messages.push("skip: the branch is alive");
            }
        }
    }

    Ok(c)
}

#[derive(Clone)]
pub struct MergeTracker {
    merged_set: Arc<Mutex<HashSet<String>>>,
}

impl MergeTracker {
    pub fn new() -> Self {
        Self {
            merged_set: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub fn track(&self, repo: &Repository, refname: &str) -> Result<()> {
        let oid = repo
            .find_reference(refname)?
            .peel_to_commit()?
            .id()
            .to_string();
        let mut set = self.merged_set.lock().unwrap();
        set.insert(oid);
        Ok(())
    }

    pub fn check_and_track(&self, repo: &Repository, base: &str, refname: &str) -> Result<bool> {
        let base_oid = repo.find_reference(base)?.peel_to_commit()?.id();
        let target_oid = repo.find_reference(refname)?.peel_to_commit()?.id();
        let target_oid_string = target_oid.to_string();

        // I know the locking is ugly. I'm trying to hold the lock as short as possible.
        // Operations against `repo` take long time up to several seconds when the disk is slow.
        {
            let set = self.merged_set.lock().unwrap().clone();
            if set.contains(&target_oid_string) {
                return Ok(true);
            }

            for merged in set.iter() {
                let merged = Oid::from_str(merged)?;
                //         B  A
                //     *--*--*
                //   /        \
                // *--*--*--*--* base
                // In this diagram, `$(git merge-base A B) == B`.
                // When we're sure that A is merged into base, then we can safely conclude that
                // B is also merged into base.
                if repo.merge_base(merged, target_oid)? == target_oid {
                    let mut set = self.merged_set.lock().unwrap();
                    set.insert(target_oid_string);
                    return Ok(true);
                }
            }
        }

        if is_merged_by_rev_list(repo, base, refname)? {
            let mut set = self.merged_set.lock().unwrap();
            set.insert(target_oid_string);
            return Ok(true);
        }

        let merge_base = repo.merge_base(base_oid, target_oid)?.to_string();
        if is_squash_merged(repo, &merge_base, base, refname)? {
            let mut set = self.merged_set.lock().unwrap();
            set.insert(target_oid_string);
            return Ok(true);
        }

        Ok(false)
    }
}

/// Source: https://stackoverflow.com/a/56026209
fn is_squash_merged(
    repo: &Repository,
    merge_base: &str,
    base: &str,
    refname: &str,
) -> Result<bool> {
    let tree = repo
        .revparse_single(&format!("{}^{{tree}}", refname))?
        .peel_to_tree()?;
    let tmp_sig = Signature::now("git-trim", "git-trim@squash.merge.test.local")?;
    let dangling_commit = repo.commit(
        None,
        &tmp_sig,
        &tmp_sig,
        "git-trim: squash merge test",
        &tree,
        &[&repo.find_commit(Oid::from_str(merge_base)?)?],
    )?;

    is_merged_by_rev_list(repo, base, &dangling_commit.to_string())
}
