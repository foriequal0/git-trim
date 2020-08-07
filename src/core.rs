use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use git2::{Oid, Repository, Signature};
use log::*;

use crate::args::DeleteFilter;
use crate::branch::{get_fetch_upstream, LocalBranch, Refname, RemoteBranch, RemoteTrackingBranch};
use crate::subprocess::{get_worktrees, is_merged_by_rev_list, RemoteHead};
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
            match branch {
                ClassifiedBranch::MergedLocal(local)
                | ClassifiedBranch::Stray(local)
                | ClassifiedBranch::Diverged { local, .. } => result.push(local),
                _ => {}
            }
        }
        result
    }

    pub fn remotes_to_delete(&self) -> Vec<&RemoteBranch> {
        let mut result = Vec::new();
        for branch in &self.to_delete {
            match branch {
                ClassifiedBranch::MergedRemote(remote)
                | ClassifiedBranch::Diverged { remote, .. } => result.push(remote),
                _ => {}
            }
        }
        result
    }
}

impl TrimPlan {
    pub fn preserve(
        &mut self,
        repo: &Repository,
        preserved_refnames: &HashSet<String>,
        reason: &'static str,
    ) -> Result<()> {
        let mut preserve = Vec::new();
        for branch in &self.to_delete {
            let contained = match &branch {
                ClassifiedBranch::MergedLocal(local) | ClassifiedBranch::Stray(local) => {
                    preserved_refnames.contains(&local.refname)
                }
                ClassifiedBranch::MergedRemote(remote) => {
                    match RemoteTrackingBranch::from_remote_branch(repo, remote)? {
                        Some(remote_tracking)
                            if preserved_refnames.contains(&remote_tracking.refname) =>
                        {
                            true
                        }
                        _ => false,
                    }
                }
                ClassifiedBranch::Diverged { local, remote } => {
                    let preserve_local = preserved_refnames.contains(&local.refname);
                    let preserve_remote =
                        match RemoteTrackingBranch::from_remote_branch(repo, remote)? {
                            Some(remote_tracking)
                                if preserved_refnames.contains(&remote_tracking.refname) =>
                            {
                                true
                            }
                            _ => false,
                        };
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
    pub fn preserve_non_heads_remotes(&mut self) {
        let mut preserve = Vec::new();

        for branch in &self.to_delete {
            let remote = match branch {
                ClassifiedBranch::MergedRemote(remote)
                | ClassifiedBranch::Diverged { remote, .. } => remote,
                _ => continue,
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
    }

    pub fn preserve_worktree(&mut self, repo: &Repository) -> Result<()> {
        let worktrees = get_worktrees(repo)?;
        let mut preserve = Vec::new();
        for branch in &self.to_delete {
            let local = match branch {
                ClassifiedBranch::MergedLocal(local)
                | ClassifiedBranch::Stray(local)
                | ClassifiedBranch::Diverged { local, .. } => local,
                _ => continue,
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

    pub fn apply_filter(&mut self, filter: &DeleteFilter) -> Result<()> {
        trace!("Before filter: {:#?}", self.to_delete);
        trace!("Applying filter: {:?}", filter);

        let mut preserve = Vec::new();

        for branch in &self.to_delete {
            let delete = match branch {
                ClassifiedBranch::MergedLocal(_) => filter.delete_merged_local(),
                ClassifiedBranch::Stray(_) => filter.delete_stray(),
                ClassifiedBranch::MergedRemote(remote) => {
                    filter.delete_merged_remote(&remote.remote)
                }
                ClassifiedBranch::Diverged { remote, .. } => filter.delete_diverged(&remote.remote),
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
            let local = match branch {
                ClassifiedBranch::MergedLocal(local)
                | ClassifiedBranch::Stray(local)
                | ClassifiedBranch::Diverged { local, .. } => local,
                _ => continue,
            };
            if local == &head_branch {
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
        for branch in &self.preserved {
            match &branch.branch {
                ClassifiedBranch::MergedLocal(local)
                | ClassifiedBranch::Stray(local)
                | ClassifiedBranch::Diverged { local, .. } => {
                    if local == target {
                        return Some(branch);
                    }
                }
                _ => {}
            }
        }
        None
    }

    pub fn get_preserved_remote(&self, target: &RemoteBranch) -> Option<&Preserved> {
        for branch in &self.preserved {
            match &branch.branch {
                ClassifiedBranch::MergedRemote(remote)
                | ClassifiedBranch::Diverged { remote, .. } => {
                    if remote == target {
                        return Some(branch);
                    }
                }
                _ => {}
            }
        }
        None
    }
}

#[derive(Debug, Clone)]
pub struct MergeState<B> {
    branch: B,
    commit: String,
    merged: bool,
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
    MergedRemote(RemoteBranch),
    Diverged {
        local: LocalBranch,
        remote: RemoteBranch,
    },
}

impl ClassifiedBranch {
    pub fn class(&self) -> &'static str {
        match self {
            ClassifiedBranch::MergedLocal(_) | ClassifiedBranch::MergedRemote(_) => "merged",
            ClassifiedBranch::Stray(_) => "stray",
            ClassifiedBranch::Diverged { .. } => "diverged",
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
    let fetch = if let Some(fetch) = get_fetch_upstream(&git.repo, &git.config, branch)? {
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
                c.messages.push("local is merged");
                c.result
                    .insert(ClassifiedBranch::MergedLocal(branch.clone()));
                if upstream.merged {
                    c.messages.push("fetch upstream is merged");
                    c.result.insert(ClassifiedBranch::MergedRemote(
                        upstream.branch.to_remote_branch(&git.repo)?,
                    ));
                } else {
                    c.messages.push("fetch upstream is diverged");
                    c.result.insert(ClassifiedBranch::Diverged {
                        local: branch.clone(),
                        remote: upstream.branch.to_remote_branch(&git.repo)?,
                    });
                }
            } else if upstream.merged {
                c.messages.push("upstream is merged, but the local strays");
                c.result.insert(ClassifiedBranch::Stray(branch.clone()));
                c.result.insert(ClassifiedBranch::MergedRemote(
                    upstream.branch.to_remote_branch(&git.repo)?,
                ));
            }
        }

        // `hub-cli` sets config `branch.{branch_name}.remote` as URL without `remote.{remote}` entry.
        // so `get_push_upstream` and `get_fetch_upstream` returns None.
        // However we can try manual classification without `remote.{remote}` entry.
        None => {
            let remote = config::get_remote_raw(&git.config, branch)?
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
                    c.result
                        .insert(ClassifiedBranch::MergedLocal(branch.clone()));
                    c.result
                        .insert(ClassifiedBranch::MergedRemote(RemoteBranch {
                            remote,
                            refname: merge,
                        }));
                }
                (true, Some(_)) => {
                    c.messages.push(
                        "merged local, diverged upstream: the branch is merged, but upstream is diverged",
                    );
                    c.result.insert(ClassifiedBranch::Diverged {
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

    pub fn track<T>(&self, repo: &Repository, branch: &T) -> Result<()>
    where
        T: Refname,
    {
        let oid = repo
            .find_reference(branch.refname())?
            .peel_to_commit()?
            .id()
            .to_string();
        let mut set = self.merged_set.lock().unwrap();
        set.insert(oid);
        Ok(())
    }

    pub fn check_and_track<T>(
        &self,
        repo: &Repository,
        base: &str,
        branch: &T,
    ) -> Result<MergeState<T>>
    where
        T: Refname + Clone,
    {
        let base_commit_id = repo.find_reference(base)?.peel_to_commit()?.id();
        let target_commit_id = repo
            .find_reference(branch.refname())?
            .peel_to_commit()?
            .id();
        let target_commit_id_string = target_commit_id.to_string();

        // I know the locking is ugly. I'm trying to hold the lock as short as possible.
        // Operations against `repo` take long time up to several seconds when the disk is slow.
        {
            let set = self.merged_set.lock().unwrap().clone();
            if set.contains(&target_commit_id_string) {
                return Ok(MergeState {
                    merged: true,
                    commit: target_commit_id_string,
                    branch: branch.clone(),
                });
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
                if repo.merge_base(merged, target_commit_id)? == target_commit_id {
                    let mut set = self.merged_set.lock().unwrap();
                    set.insert(target_commit_id_string.clone());
                    return Ok(MergeState {
                        merged: true,
                        commit: target_commit_id_string,
                        branch: branch.clone(),
                    });
                }
            }
        }

        if is_merged_by_rev_list(repo, base, branch.refname())? {
            let mut set = self.merged_set.lock().unwrap();
            set.insert(target_commit_id_string.clone());
            return Ok(MergeState {
                merged: true,
                commit: target_commit_id_string,
                branch: branch.clone(),
            });
        }

        let merge_base = repo
            .merge_base(base_commit_id, target_commit_id)?
            .to_string();
        if is_squash_merged(repo, &merge_base, base, branch.refname())? {
            let mut set = self.merged_set.lock().unwrap();
            set.insert(target_commit_id_string.clone());
            return Ok(MergeState {
                merged: true,
                commit: target_commit_id_string,
                branch: branch.clone(),
            });
        }

        Ok(MergeState {
            merged: false,
            commit: target_commit_id_string,
            branch: branch.clone(),
        })
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
