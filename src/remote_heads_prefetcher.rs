use std::convert::TryFrom;
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use crossbeam_channel::unbounded;
use git2::BranchType;

use crate::{config, subprocess, ForceSendSync, Git, LocalBranch, RemoteHead};

pub enum RemoteHeadsPrefetcher {
    Fetching(JoinHandle<Vec<Result<Vec<RemoteHead>>>>),
    Noop,
}

impl RemoteHeadsPrefetcher {
    pub fn noop() -> Self {
        RemoteHeadsPrefetcher::Noop
    }

    pub fn spawn(git: &Git) -> Result<Self> {
        let remote_urls = get_remote_urls(git)?;
        if remote_urls.is_empty() {
            return Ok(Self::Noop);
        }

        let git = ForceSendSync::new(git).as_static();
        let join_handle = std::thread::spawn(move || {
            let (branches_sender, branches_receiver) = unbounded();
            rayon::scope(move |scope| {
                for remote_url in remote_urls {
                    let branches_sender = branches_sender.clone();
                    scope.spawn(move |_| {
                        let result = subprocess::ls_remote_heads(&git.repo, &remote_url)
                            .with_context(|| format!("remote_url={}", remote_url));
                        branches_sender.send(result).unwrap();
                    });
                }
            });
            branches_receiver.iter().collect()
        });

        Ok(Self::Fetching(join_handle))
    }

    pub fn get(self) -> Result<Vec<RemoteHead>> {
        match self {
            RemoteHeadsPrefetcher::Fetching(join_handle) => {
                let mut result = Vec::new();
                for heads in join_handle.join().unwrap() {
                    result.extend(heads?);
                }
                Ok(result)
            }
            RemoteHeadsPrefetcher::Noop => Ok(Vec::new()),
        }
    }
}

fn get_remote_urls(git: &Git) -> Result<Vec<String>> {
    let mut result = Vec::new();
    for branch in git.repo.branches(Some(BranchType::Local))? {
        let local = LocalBranch::try_from(&branch?.0)?;

        let remote = if let Some(remote) = config::get_remote_name(&git.config, &local)? {
            remote
        } else {
            continue;
        };

        if config::get_remote(&git.repo, &remote)?.is_some() {
            continue;
        }

        result.push(remote);
    }

    Ok(result)
}
