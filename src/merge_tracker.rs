use std::collections::HashSet;
use std::fmt::Debug;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use git2::{Config, ErrorClass, ErrorCode, Oid, Repository, Signature};
use log::{debug, info, trace};

use crate::branch::{Refname, RemoteTrackingBranch};
use crate::subprocess::{self, is_merged_by_rev_list};

#[derive(Clone)]
pub struct MergeTracker {
    merged_set: Arc<Mutex<HashSet<String>>>,
}

#[derive(Debug, Clone)]
pub struct MergeState<B> {
    pub branch: B,
    pub commit: String,
    pub merged: bool,
}

impl MergeTracker {
    pub fn with_base_upstreams(
        repo: &Repository,
        config: &Config,
        base_upstreams: &[RemoteTrackingBranch],
    ) -> Result<Self> {
        let tracker = Self {
            merged_set: Arc::new(Mutex::new(HashSet::new())),
        };
        info!("Initializing MergeTracker");
        for base_upstream in base_upstreams {
            debug!("base_upstream: {:?}", base_upstream);
            tracker.track(repo, base_upstream)?;
        }

        for merged_local in subprocess::get_noff_merged_locals(repo, config, base_upstreams)? {
            debug!("merged_local: {:?}", merged_local);
            tracker.track(repo, &merged_local)?;
        }

        for merged_remote in subprocess::get_noff_merged_remotes(repo, base_upstreams)? {
            debug!("merged_remote: {:?}", merged_remote);
            tracker.track(repo, &merged_remote)?;
        }

        Ok(tracker)
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

        let mut set = self.merged_set.lock().expect("Unable to lock merged set");
        trace!("track: {}", oid);
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
            let set = self
                .merged_set
                .lock()
                .expect("Unable to lock merged_set")
                .clone();
            if set.contains(&target_commit_id_string) {
                debug!(
                    "tracked: {} ({})",
                    &target_commit_id_string[0..7],
                    branch.refname(),
                );
                return Ok(MergeState {
                    merged: true,
                    commit: target_commit_id_string,
                    branch: branch.clone(),
                });
            }

            for merged in &set {
                let merged_oid = Oid::from_str(merged)?;
                //         B  A
                //     *--*--*
                //   /        \
                // *--*--*--*--* base
                // In this diagram, `$(git merge-base A B) == B`.
                // When we're sure that A is merged into base, then we can safely conclude that
                // B is also merged into base.
                let noff_merged = match repo.merge_base(merged_oid, target_commit_id) {
                    Ok(merge_base) if merge_base == target_commit_id => {
                        let mut set = self.merged_set.lock().expect("Unable to lock merged_set");
                        set.insert(target_commit_id_string.clone());
                        true
                    }
                    Ok(_) => continue,
                    Err(err) if merge_base_not_found(&err) => false,
                    Err(err) => return Err(err.into()),
                };

                debug!("noff merged: ({}) -> {}", branch.refname(), &merged[0..7]);

                return Ok(MergeState {
                    merged: noff_merged,
                    commit: target_commit_id_string,
                    branch: branch.clone(),
                });
            }
        }

        if is_merged_by_rev_list(repo, base, branch.refname())? {
            let mut set = self.merged_set.lock().expect("Unable to lock merged_set");

            set.insert(target_commit_id_string.clone());
            debug!("rebase merged: {} -> {}", branch.refname(), &base);

            return Ok(MergeState {
                merged: true,
                commit: target_commit_id_string,
                branch: branch.clone(),
            });
        }

        let squash_merged = match repo.merge_base(base_commit_id, target_commit_id) {
            Ok(merge_base) => {
                let merge_base = merge_base.to_string();
                let squash_merged = is_squash_merged(repo, &merge_base, base, branch.refname())?;
                if squash_merged {
                    let mut set = self.merged_set.lock().expect("Unable to lock merged_set");
                    set.insert(target_commit_id_string.clone());
                }
                squash_merged
            }
            Err(err) if merge_base_not_found(&err) => false,
            Err(err) => return Err(err.into()),
        };

        if squash_merged {
            debug!("squash merged: {} -> {}", branch.refname(), &base);
        }
        Ok(MergeState {
            merged: squash_merged,
            commit: target_commit_id_string,
            branch: branch.clone(),
        })
    }
}

fn merge_base_not_found(err: &git2::Error) -> bool {
    err.class() == ErrorClass::Merge && err.code() == ErrorCode::NotFound
}

/// Source: <https://stackoverflow.com/a/56026209>
fn is_squash_merged(
    repo: &Repository,
    merge_base: &str,
    base: &str,
    refname: &str,
) -> Result<bool> {
    let tree = repo
        .revparse_single(&format!("{refname}^{{tree}}"))?
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
