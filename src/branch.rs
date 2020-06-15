use anyhow::{Context, Result};
use git2::{Config, Direction, Error, ErrorClass, ErrorCode, Remote, Repository};
use log::*;
use thiserror::Error;

use crate::config;
use crate::simple_glob::{expand_refspec, ExpansionSide};

#[derive(Eq, PartialEq, Ord, PartialOrd, Debug, Hash)]
pub struct RemoteTrackingBranch {
    pub refname: String,
}

impl RemoteTrackingBranch {
    pub fn new(refname: &str) -> RemoteTrackingBranch {
        assert!(refname.starts_with("refs/remotes/"));
        RemoteTrackingBranch {
            refname: refname.to_string(),
        }
    }

    pub fn from_remote_branch(
        repo: &Repository,
        remote_branch: &RemoteBranch,
    ) -> Result<Option<RemoteTrackingBranch>> {
        let remote = get_remote(repo, &remote_branch.remote)?;
        if let Some(remote) = remote {
            let refname = if let Some(expanded) = expand_refspec(
                &remote,
                &remote_branch.refname,
                Direction::Fetch,
                ExpansionSide::Right,
            )? {
                expanded
            } else {
                return Ok(None);
            };

            if repo.find_reference(&refname).is_ok() {
                return Ok(Some(RemoteTrackingBranch::new(&refname)));
            } else {
                return Ok(None);
            }
        }
        Ok(None)
    }

    pub fn remote_branch(
        &self,
        repo: &Repository,
    ) -> std::result::Result<RemoteBranch, RemoteBranchError> {
        for remote_name in repo.remotes()?.iter() {
            let remote_name = remote_name.context("non-utf8 remote name")?;
            let remote = repo.find_remote(&remote_name)?;
            if let Some(expanded) = expand_refspec(
                &remote,
                &self.refname,
                Direction::Fetch,
                ExpansionSide::Left,
            )? {
                return Ok(RemoteBranch {
                    remote: remote.name().context("non-utf8 remote name")?.to_string(),
                    refname: expanded,
                });
            }
        }
        Err(RemoteBranchError::RemoteNotFound)
    }
}

// given refspec for a remote: refs/heads/*:refs/remotes/origin
// master -> refs/remotes/origin/master
// refs/head/master -> refs/remotes/origin/master
pub fn get_fetch_upstream(
    repo: &Repository,
    config: &Config,
    refname: &str,
) -> Result<Option<RemoteTrackingBranch>> {
    let remote_name = config::get_remote(config, refname)?;
    let merge: String = if let Some(merge) = config::get_merge(config, &refname)? {
        merge
    } else {
        return Ok(None);
    };

    RemoteTrackingBranch::from_remote_branch(
        repo,
        &RemoteBranch {
            remote: remote_name.to_string(),
            refname: merge,
        },
    )
}

pub fn get_remote<'a>(repo: &'a Repository, remote_name: &str) -> Result<Option<Remote<'a>>> {
    fn error_is_missing_remote(err: &Error) -> bool {
        err.class() == ErrorClass::Config && err.code() == ErrorCode::InvalidSpec
    }

    match repo.find_remote(remote_name) {
        Ok(remote) => Ok(Some(remote)),
        Err(err) if error_is_missing_remote(&err) => Ok(None),
        Err(err) => Err(err.into()),
    }
}

// given refspec for a remote: refs/heads/*:refs/heads/*
// master -> refs/remotes/origin/master
// refs/head/master -> refs/remotes/origin/master
pub fn get_push_upstream(
    repo: &Repository,
    config: &Config,
    refname: &str,
) -> Result<Option<RemoteTrackingBranch>> {
    if let Some(remote_branch) = get_push_remote_branch(repo, config, refname)? {
        return RemoteTrackingBranch::from_remote_branch(repo, &remote_branch);
    }
    Ok(None)
}

#[derive(Eq, PartialEq, Ord, PartialOrd, Clone, Hash, Debug)]
pub struct RemoteBranch {
    pub remote: String,
    pub refname: String,
}

impl std::fmt::Display for RemoteBranch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}, {}", self.remote, self.refname)
    }
}

#[derive(Error, Debug)]
pub enum RemoteBranchError {
    #[error("anyhow error")]
    AnyhowError(#[from] anyhow::Error),
    #[error("libgit2 internal error")]
    GitError(#[from] git2::Error),
    #[error("remote with matching refspec not found")]
    RemoteNotFound,
}

fn get_push_remote_branch(
    repo: &Repository,
    config: &Config,
    refname: &str,
) -> Result<Option<RemoteBranch>> {
    let reference = repo.find_reference(refname)?;
    if !reference.is_branch() || reference.is_remote() {
        return Err(anyhow::anyhow!("Not a local branch: {}", refname));
    }
    let refname = reference.name().context("non utf-8 refname")?;

    let remote_name = config::get_push_remote(config, refname)?;
    if let Some(remote) = get_remote(repo, &remote_name)? {
        let refname = if let Some(expanded) =
            expand_refspec(&remote, &refname, Direction::Push, ExpansionSide::Right)?
        {
            expanded
        } else {
            return Ok(None);
        };

        if repo.find_reference(&refname).is_ok() {
            return Ok(Some(RemoteBranch {
                remote: remote_name.to_string(),
                refname,
            }));
        } else {
            return Ok(None);
        }
    }

    let push_default = config::get(config, "push.default")
        .with_default(&String::from("simple"))
        .read()?
        .expect("has default");

    match push_default.as_str() {
        "current" => Ok(Some(RemoteBranch {
            remote: remote_name.to_string(),
            refname: refname.to_string(),
        })),
        "upstream" | "tracking" | "simple" | "matching" => {
            if let Some(merge) = config::get_merge(config, &refname)? {
                Ok(Some(RemoteBranch {
                    remote: remote_name.clone(),
                    refname: merge,
                }))
            } else {
                warn!("The current branch {} has no upstream branch.", refname);
                Ok(None)
            }
        }
        "nothing" => unimplemented!("push.default=nothing is not implemented."),
        _ => panic!("unexpected config push.default"),
    }
}
