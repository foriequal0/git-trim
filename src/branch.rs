use std::convert::TryFrom;

use anyhow::{Context, Result};
use git2::{
    Branch, Config, Direction, Error, ErrorClass, ErrorCode, Reference, Remote, Repository,
};
use log::*;
use thiserror::Error;

use crate::config;
use crate::simple_glob::{expand_refspec, ExpansionSide};

#[derive(Eq, PartialEq, Ord, PartialOrd, Debug, Hash, Clone)]
pub struct LocalBranch {
    pub refname: String,
}

impl LocalBranch {
    pub fn new(refname: &str) -> Self {
        assert!(refname.starts_with("refs/heads/"));
        Self {
            refname: refname.to_string(),
        }
    }

    pub fn short_name(&self) -> &str {
        &self.refname["refs/heads/".len()..]
    }
}

impl<'repo> TryFrom<&git2::Branch<'repo>> for LocalBranch {
    type Error = anyhow::Error;

    fn try_from(branch: &Branch<'repo>) -> Result<Self> {
        let refname = branch.get().name().context("non-utf8 branch ref")?;
        Ok(Self::new(refname))
    }
}

impl<'repo> TryFrom<&git2::Reference<'repo>> for LocalBranch {
    type Error = anyhow::Error;

    fn try_from(reference: &Reference<'repo>) -> Result<Self> {
        if !reference.is_branch() {
            anyhow::anyhow!("Reference {:?} is not a branch", reference.name());
        }

        let refname = reference.name().context("non-utf8 reference name")?;
        Ok(Self::new(refname))
    }
}

#[derive(Eq, PartialEq, Ord, PartialOrd, Debug, Hash, Clone)]
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
        let remote = get_remote_entry(repo, &remote_branch.remote)?;
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

    pub fn to_remote_branch(
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

pub fn get_fetch_upstream(
    repo: &Repository,
    config: &Config,
    branch: &LocalBranch,
) -> Result<Option<RemoteTrackingBranch>> {
    let remote_name = config::get_remote(config, branch)?;
    let merge: String = if let Some(merge) = config::get_merge(config, &branch)? {
        merge
    } else {
        return Ok(None);
    };

    RemoteTrackingBranch::from_remote_branch(
        repo,
        &RemoteBranch {
            remote: remote_name,
            refname: merge,
        },
    )
}

pub fn get_remote_entry<'a>(repo: &'a Repository, remote_name: &str) -> Result<Option<Remote<'a>>> {
    fn error_is_missing_remote(err: &Error) -> bool {
        err.class() == ErrorClass::Config && err.code() == ErrorCode::InvalidSpec
    }

    match repo.find_remote(remote_name) {
        Ok(remote) => Ok(Some(remote)),
        Err(err) if error_is_missing_remote(&err) => Ok(None),
        Err(err) => Err(err.into()),
    }
}

pub fn get_push_upstream(
    repo: &Repository,
    config: &Config,
    branch: &LocalBranch,
) -> Result<Option<RemoteTrackingBranch>> {
    if let Some(remote_branch) = get_explicit_push_remote_branch(repo, config, branch)? {
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

fn get_explicit_push_remote_branch(
    repo: &Repository,
    config: &Config,
    branch: &LocalBranch,
) -> Result<Option<RemoteBranch>> {
    let remote_name = config::get_push_remote(config, branch)?;
    if let Some(remote) = get_remote_entry(repo, &remote_name)? {
        let refname = if let Some(expanded) = expand_refspec(
            &remote,
            &branch.refname,
            Direction::Push,
            ExpansionSide::Right,
        )? {
            expanded
        } else {
            // `git push` will fallback to `push.default`.
            // However we'll stop here since we want to distinguish explicit tracking
            // and implicit tracking.
            return Ok(None);
        };

        if repo.find_reference(&refname).is_ok() {
            return Ok(Some(RemoteBranch {
                remote: remote_name,
                refname,
            }));
        } else {
            return Ok(None);
        }
    }

    let push_default = config::get(config, "push.default")
        .with_default(String::from("simple"))
        .read()?
        .expect("has default");

    match push_default.as_str() {
        "current" => Ok(Some(RemoteBranch {
            remote: remote_name,
            refname: branch.refname.clone(),
        })),
        "upstream" | "tracking" | "simple" | "matching" => {
            if let Some(merge) = config::get_merge(config, &branch)? {
                Ok(Some(RemoteBranch {
                    remote: remote_name,
                    refname: merge,
                }))
            } else {
                warn!(
                    "The current branch {} has no upstream branch.",
                    branch.refname
                );
                Ok(None)
            }
        }
        "nothing" => unimplemented!("push.default=nothing is not implemented."),
        _ => panic!("unexpected config push.default"),
    }
}
