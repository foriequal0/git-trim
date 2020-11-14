use std::thread::JoinHandle;

use anyhow::{Context, Result};
use git2::Repository;
use log::*;
use rayon::prelude::*;

use crate::{ls_remote_head, ForceSendSync, RemoteHead, RemoteTrackingBranch};
use std::collections::HashMap;

pub struct RemoteHeadChangeChecker {
    join_handle: JoinHandle<Result<Vec<RemoteHead>>>,
}

impl RemoteHeadChangeChecker {
    pub fn spawn() -> Result<Self> {
        let join_handle = {
            let repo = ForceSendSync::new(Repository::open_from_env()?);
            let remotes = {
                let mut tmp = Vec::new();
                for remote_name in repo.remotes()?.iter() {
                    let remote_name = remote_name.context("non-utf8 remote name")?;
                    tmp.push(remote_name.to_owned())
                }
                tmp
            };
            std::thread::spawn(move || {
                remotes
                    .par_iter()
                    .map(|remote_name| ls_remote_head(&repo, remote_name))
                    .collect()
            })
        };
        Ok(Self { join_handle })
    }

    pub fn check_and_notify(self, repo: &Repository) -> Result<()> {
        let fetched_remote_heads_raw = self.join_handle.join().unwrap()?;
        let mut fetched_remote_heads = HashMap::new();
        for remote_head in fetched_remote_heads_raw.into_iter() {
            fetched_remote_heads.insert(remote_head.remote.clone(), remote_head);
        }

        struct OutOfSync<'a> {
            remote: &'a str,
            cached: Option<String>,
            fetched: &'a str,
        }
        let mut out_of_sync = Vec::new();
        let remotes = repo.remotes()?;
        for remote in remotes.iter() {
            let remote = remote.context("non-utf8 remote name")?;
            let reference = match repo.find_reference(&format!("refs/remotes/{}/HEAD", remote)) {
                Ok(reference) => reference,
                Err(_) => {
                    out_of_sync.push(OutOfSync {
                        remote,
                        cached: None,
                        fetched: &fetched_remote_heads[remote].refname,
                    });
                    continue;
                }
            };
            // git symbolic-ref refs/remotes/*/HEAD
            let resolved = match reference.resolve() {
                Ok(resolved) => resolved,
                Err(_) => {
                    debug!(
                        "Reference {:?} is expected to be an symbolic ref, but it isn't",
                        reference.name()
                    );
                    continue;
                }
            };
            let refname = resolved.name().context("non utf-8 reference name")?;

            let remote_head = RemoteTrackingBranch::new(refname).to_remote_branch(repo)?;
            let fetched_remote_head = &fetched_remote_heads[remote];
            if remote_head.refname != fetched_remote_head.refname {
                out_of_sync.push(OutOfSync {
                    remote,
                    cached: Some(remote_head.refname),
                    fetched: &fetched_remote_head.refname,
                });
            }
        }

        if out_of_sync.is_empty() {
            return Ok(());
        }

        eprintln!(
            "You are using default base branches, which is deduced from `refs/remotes/*/HEAD`s."
        );
        eprintln!("However, they seems to be out of sync.");
        for entry in &out_of_sync {
            if let Some(cached) = &entry.cached {
                eprintln!(
                    " * {remote}: {before} -> {after}",
                    remote = entry.remote,
                    before = cached,
                    after = entry.fetched
                );
            } else {
                eprintln!(
                    " * {remote}: None -> {after}",
                    remote = entry.remote,
                    after = entry.fetched
                );
            }
        }
        eprintln!("You can sync them with these commands:");
        for entry in &out_of_sync {
            eprintln!(" > git remote set-head {} --auto", entry.remote);
        }
        eprintln!(
            r#"Or you can set base branches manually:
 * `git config trim.bases develop,master` will set base branches for git-trim for a repository.
 * `git config --global trim.bases develop,master` will set base branches for `git-trim` globally.
 * `git trim --bases develop,master` will temporarily set base branches for `git-trim`"#
        );

        Ok(())
    }
}
