use anyhow::{Context, Result};
use git2::{BranchType, Config, Direction, Repository};
use log::*;

use crate::config;
use crate::config::ConfigValue;
use crate::simple_glob::{expand_refspec, ExpansionSide};

// given refspec for a remote: refs/heads/*:refs/remotes/origin
// master -> refs/remotes/origin/master
// refs/head/master -> refs/remotes/origin/master
pub fn get_fetch_remote_ref(
    repo: &Repository,
    config: &Config,
    branch: &str,
) -> Result<Option<String>> {
    let remote_name = config::get_remote(config, branch)?;
    get_remote_ref(repo, config, &remote_name, branch)
}

fn get_remote_ref(
    repo: &Repository,
    config: &Config,
    remote_name: &str,
    branch: &str,
) -> Result<Option<String>> {
    let remote = repo.find_remote(remote_name)?;
    let key = format!("branch.{}.merge", branch);
    let ref_on_remote: ConfigValue<String> =
        if let Some(ref_on_remote) = config::get(config, &key).read()? {
            ref_on_remote
        } else {
            return Ok(None);
        };
    assert!(
        ref_on_remote.starts_with("refs/"),
        "'git config branch.{}.merge' should start with 'refs/'",
        branch
    );

    if let Some(expanded) = expand_refspec(
        &remote,
        &ref_on_remote,
        Direction::Fetch,
        ExpansionSide::Right,
    )? {
        // TODO: is this necessary?
        let exists = repo.find_reference(&expanded).is_ok();
        if exists {
            Ok(Some(expanded))
        } else {
            Ok(None)
        }
    } else {
        Ok(None)
    }
}

// given refspec for a remote: refs/heads/*:refs/heads/*
// master -> refs/remotes/origin/master
// refs/head/master -> refs/remotes/origin/master
pub fn get_push_remote_ref(
    repo: &Repository,
    config: &Config,
    branch: &str,
) -> Result<Option<String>> {
    if let Some(RefOnRemote {
        remote_name,
        refname,
    }) = get_push_ref_on_remote(repo, config, branch)?
    {
        if let Some(remote_ref) = get_remote_ref(repo, config, &remote_name, &refname)? {
            return Ok(Some(remote_ref));
        }
    }
    Ok(None)
}

#[derive(Eq, PartialEq, Clone)]
pub struct RefOnRemote {
    pub remote_name: String,
    pub refname: String,
}

fn get_push_ref_on_remote(
    repo: &Repository,
    config: &Config,
    branch: &str,
) -> Result<Option<RefOnRemote>> {
    let remote_name = config::get_push_remote(config, branch)?;

    let remote = repo.find_remote(&remote_name)?;
    let reference = repo
        .find_branch(branch, BranchType::Local)?
        .into_reference();
    let refname = reference.name().context("non utf-8 refname")?;
    if let Some(push_on_remote) =
        expand_refspec(&remote, refname, Direction::Push, ExpansionSide::Right)?
    {
        return Ok(Some(RefOnRemote {
            remote_name: remote_name.to_string(),
            refname: push_on_remote,
        }));
    }

    let push_default = config::get(config, "push.default")
        .with_default(&String::from("simple"))
        .read()?
        .expect("has default");

    match push_default.as_str() {
        "current" => Ok(Some(RefOnRemote {
            remote_name: remote_name.to_string(),
            refname: branch.to_string(),
        })),
        "upstream" | "tracking" | "simple" => {
            if let Some(merge) = config::get(config, &format!("branch.{}.merge", branch))
                .parse_with(|ref_on_remote| {
                    Ok(RefOnRemote {
                        remote_name: remote_name.clone(),
                        refname: ref_on_remote.to_string(),
                    })
                })?
            {
                Ok(Some(merge.clone()))
            } else {
                warn!("The current branch {} has no upstream branch.", branch);
                Ok(None)
            }
        }
        "nothing" | "matching" => {
            unimplemented!("push.default=nothing|matching is not implemented.")
        }
        _ => panic!("unexpected config push.default"),
    }
}

pub fn get_ref_on_remote_from_remote_ref(
    repo: &Repository,
    remote_ref: &str,
) -> Result<RefOnRemote> {
    assert!(remote_ref.starts_with("refs/remotes/"));
    for remote_name in repo.remotes()?.iter() {
        let remote_name = remote_name.context("non-utf8 remote name")?;
        let remote = repo.find_remote(&remote_name)?;
        if let Some(expanded) =
            expand_refspec(&remote, remote_ref, Direction::Fetch, ExpansionSide::Left)?
        {
            return Ok(RefOnRemote {
                remote_name: remote.name().context("non-utf8 remote name")?.to_string(),
                refname: expanded,
            });
        }
    }
    unreachable!("matching refspec is not found");
}
