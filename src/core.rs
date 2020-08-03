use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use git2::{Oid, Repository, Signature};
use log::*;

use crate::args::DeleteFilter;
use crate::branch::{get_fetch_upstream, LocalBranch, RemoteBranch, RemoteTrackingBranch};
use crate::subprocess::{get_worktrees, is_merged_by_rev_list};
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
                ClassifiedBranch::MergedLocal(local) | ClassifiedBranch::StrayLocal(local) => {
                    result.push(local)
                }
                _ => {}
            }
        }
        result
    }

    pub fn remotes_to_delete(&self) -> Vec<&RemoteBranch> {
        let mut result = Vec::new();
        for branch in &self.to_delete {
            match branch {
                ClassifiedBranch::MergedRemote(remote) | ClassifiedBranch::StrayRemote(remote) => {
                    result.push(remote)
                }
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
            match branch {
                ClassifiedBranch::MergedLocal(local) | ClassifiedBranch::StrayLocal(local) => {
                    if preserved_refnames.contains(&local.refname) {
                        preserve.push(Preserved {
                            branch: branch.clone(),
                            reason: reason.to_owned(),
                        })
                    }
                }
                ClassifiedBranch::MergedRemote(remote) | ClassifiedBranch::StrayRemote(remote) => {
                    if let Some(remote_tracking) =
                        RemoteTrackingBranch::from_remote_branch(repo, remote)?
                    {
                        if preserved_refnames.contains(&remote_tracking.refname) {
                            preserve.push(Preserved {
                                branch: branch.clone(),
                                reason: reason.to_owned(),
                            })
                        }
                    }
                }
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
    pub fn preserve_non_heads_remotes(&mut self) {
        let mut preserve = Vec::new();

        for branch in &self.to_delete {
            let remote = match branch {
                ClassifiedBranch::MergedRemote(remote) | ClassifiedBranch::StrayRemote(remote) => {
                    remote
                }
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
                ClassifiedBranch::MergedLocal(local) | ClassifiedBranch::StrayLocal(local) => local,
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
                ClassifiedBranch::StrayLocal(_) => filter.delete_stray_local(),
                ClassifiedBranch::MergedRemote(remote) => {
                    filter.delete_merged_remote(&remote.remote)
                }
                ClassifiedBranch::StrayRemote(remote) => filter.delete_stray_remote(&remote.remote),
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
                ClassifiedBranch::MergedLocal(local) | ClassifiedBranch::StrayLocal(local) => local,
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
                ClassifiedBranch::MergedLocal(local) | ClassifiedBranch::StrayLocal(local) => {
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
                ClassifiedBranch::MergedRemote(remote) | ClassifiedBranch::StrayRemote(remote) => {
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
    merged: bool,
}

pub struct Classification {
    pub local: MergeState<LocalBranch>,
    pub fetch: Option<MergeState<RemoteTrackingBranch>>,
    pub messages: Vec<&'static str>,
    pub result: HashSet<ClassifiedBranch>,
}

impl Classification {
    fn merged_or_stray_remote(
        &mut self,
        repo: &Repository,
        merge_state: &MergeState<RemoteTrackingBranch>,
    ) -> Result<()> {
        if merge_state.merged {
            self.messages
                .push("fetch upstream is merged, but forget to delete");
            self.merged_remote(repo, &merge_state.branch)
        } else {
            self.messages.push("fetch upstream is not merged");
            self.stray_remote(repo, &merge_state.branch)
        }
    }

    fn merged_remote(&mut self, repo: &Repository, upstream: &RemoteTrackingBranch) -> Result<()> {
        self.result.insert(ClassifiedBranch::MergedRemote(
            upstream.to_remote_branch(&repo)?,
        ));
        Ok(())
    }

    fn stray_remote(&mut self, repo: &Repository, upstream: &RemoteTrackingBranch) -> Result<()> {
        self.result.insert(ClassifiedBranch::StrayRemote(
            upstream.to_remote_branch(&repo)?,
        ));
        Ok(())
    }
}

#[derive(Hash, Eq, PartialEq, Debug, Clone)]
pub enum ClassifiedBranch {
    MergedLocal(LocalBranch),
    StrayLocal(LocalBranch),
    MergedRemote(RemoteBranch),
    StrayRemote(RemoteBranch),
}

impl ClassifiedBranch {
    pub fn class(&self) -> &'static str {
        match self {
            ClassifiedBranch::MergedLocal(_) | ClassifiedBranch::MergedRemote(_) => "merged",
            ClassifiedBranch::StrayLocal(_) | ClassifiedBranch::StrayRemote(_) => "stray",
        }
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
    let local = {
        let merged = merge_tracker.check_and_track(&git.repo, &base.refname, &branch.refname)?;
        MergeState {
            branch: branch.clone(),
            merged,
        }
    };
    let fetch = if let Some(fetch) = get_fetch_upstream(&git.repo, &git.config, branch)? {
        let merged = merge_tracker.check_and_track(&git.repo, &base.refname, &fetch.refname)?;
        Some(MergeState {
            branch: fetch,
            merged,
        })
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
                c.merged_or_stray_remote(&git.repo, &upstream)?;
            } else if upstream.merged {
                c.messages.push("upstream is merged, but the local strays");
                c.result
                    .insert(ClassifiedBranch::StrayLocal(branch.clone()));
                c.merged_remote(&git.repo, &upstream.branch)?;
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
            let upstream_is_exists = remote_heads_per_url.contains_key(&remote)
                && remote_heads_per_url[&remote].contains(&merge);

            if upstream_is_exists && local.merged {
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
            } else if local.merged {
                c.messages
                    .push("merged local: the branch is merged, and deleted");
                c.result
                    .insert(ClassifiedBranch::MergedLocal(branch.clone()));
            } else if !upstream_is_exists {
                c.messages
                    .push("the branch is not merged but the remote is gone somehow");
                c.result
                    .insert(ClassifiedBranch::StrayLocal(branch.clone()));
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
