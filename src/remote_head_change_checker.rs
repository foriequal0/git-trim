use std::thread::JoinHandle;

use anyhow::{Context, Result};
use git2::Repository;
use log::*;
use rayon::prelude::*;

use crate::{ls_remote_head, ForceSendSync, RemoteHead, RemoteTrackingBranch};

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
        let mut fetched_remote_heads: Vec<RemoteHead> = Vec::new();
        for remote_head in fetched_remote_heads_raw.into_iter() {
            fetched_remote_heads.push(remote_head);
        }

        let mut out_of_sync = Vec::new();
        for reference in repo.references_glob("refs/remotes/*/HEAD")? {
            let reference = reference?;
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

            let fetch_remote_head = fetched_remote_heads
                .iter()
                .find(|x| x.remote == remote_head.remote);
            if let Some(fetched_remote_head) = fetch_remote_head {
                let matches = fetched_remote_heads
                    .iter()
                    .any(|x| x.remote == remote_head.remote && x.refname == remote_head.refname);
                if !matches {
                    out_of_sync.push((remote_head, fetched_remote_head))
                }
            }
        }

        if out_of_sync.is_empty() {
            return Ok(());
        }

        eprintln!(
            "You are using default base branches, which is deduced from `refs/remotes/*/HEAD`s."
        );
        eprintln!("However, they seems to be out of sync.");
        for (remote_head, fetched_remote_head) in &out_of_sync {
            eprintln!(
                " * {remote}: {before} -> {after}",
                remote = remote_head.remote,
                before = remote_head.refname,
                after = fetched_remote_head.refname
            );
        }
        eprintln!("You can sync them with these commands:");
        for (remote_head, _) in &out_of_sync {
            eprintln!(
                " > git remote set-head {remote} --auto",
                remote = remote_head.remote,
            );
        }
        eprintln!(
            r#"Or you can set base branches manually:
 * `git config trim.bases develop,main` will set base branches for git-trim for a repository.
 * `git config --global trim.bases develop,main` will set base branches for `git-trim` globally.
 * `git trim --bases develop,main` will temporarily set base branches for `git-trim`"#
        );

        Ok(())
    }
}
