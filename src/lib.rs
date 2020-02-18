pub mod args;
pub mod config;
mod remote_ref;
mod simple_glob;
mod subprocess;

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use git2::{BranchType, Config, Direction, Repository};
use log::*;

use crate::args::{Category, DeleteFilter};
use crate::remote_ref::{get_fetch_remote_ref, get_push_remote_ref};
use crate::simple_glob::{expand_refspec, ExpansionSide};
pub use crate::subprocess::remote_update;

#[derive(Default, Eq, PartialEq, Debug)]
pub struct MergedOrGone {
    // local branches
    pub merged_locals: HashSet<String>,
    pub gone_locals: HashSet<String>,

    pub kept_back_locals: HashSet<String>,

    /// remote refs
    pub merged_remotes: HashSet<String>,
    pub gone_remotes: HashSet<String>,
}

impl MergedOrGone {
    pub fn adjust_not_to_detach(&mut self, repo: &Repository) -> Result<()> {
        if repo.head_detached()? {
            return Ok(());
        }
        let head = repo.head()?;
        let head_name = head.name().context("non-utf8 head ref name")?;
        assert!(head_name.starts_with("refs/heads/"));
        let head_name = &head_name["refs/heads/".len()..];

        if self.merged_locals.contains(head_name) {
            self.merged_locals.remove(head_name);
            self.kept_back_locals.insert(head_name.to_string());
        }
        if self.gone_locals.contains(head_name) {
            self.gone_locals.remove(head_name);
            self.kept_back_locals.insert(head_name.to_string());
        }
        Ok(())
    }

    pub fn print_summary(&self, filter: &DeleteFilter) {
        fn print(branches: &HashSet<String>, filter: &DeleteFilter, category: Category) {
            if filter.contains(&category) && !branches.is_empty() {
                println!("Delete {}:", category);
                for branch in branches {
                    println!("  {}", branch);
                }
            } else if !branches.is_empty() {
                println!("Skip {}:", category);
                for branch in branches {
                    println!("  {}", branch);
                }
            }
        }
        print(&self.merged_locals, filter, Category::MergedLocal);
        print(&self.merged_remotes, filter, Category::MergedRemote);

        if !self.kept_back_locals.is_empty() {
            println!("Kept back not to become detached HEAD:");
            for branch in &self.kept_back_locals {
                println!("  {}", branch);
            }
        }

        print(&self.gone_locals, filter, Category::GoneLocal);
        print(&self.gone_remotes, filter, Category::GoneRemote);
    }

    pub fn get_local_branches_to_delete(&self, filter: &DeleteFilter) -> Vec<&str> {
        let mut result = Vec::new();
        if filter.contains(&Category::MergedLocal) {
            result.extend(self.merged_locals.iter().map(String::as_str))
        }
        if filter.contains(&Category::GoneLocal) {
            result.extend(self.gone_locals.iter().map(String::as_str))
        }
        result
    }

    pub fn get_remote_refs_to_delete(&self, filter: &DeleteFilter) -> Vec<&str> {
        let mut result = Vec::new();
        if filter.contains(&Category::MergedRemote) {
            result.extend(self.merged_remotes.iter().map(String::as_str))
        }
        if filter.contains(&Category::GoneLocal) {
            result.extend(self.gone_remotes.iter().map(String::as_str))
        }
        result
    }
}

pub fn get_merged_or_gone(repo: &Repository, config: &Config, base: &str) -> Result<MergedOrGone> {
    let base_remote_ref = resolve_config_base_ref(repo, config, base)?;
    let mut result = MergedOrGone::default();
    // Fast filling ff merged branches
    let noff_merged_locals = subprocess::get_noff_merged_locals(repo, config, &base_remote_ref)?;
    result.merged_locals.extend(noff_merged_locals.clone());

    let mut merged_locals = HashSet::new();
    merged_locals.extend(noff_merged_locals);

    for branch in repo.branches(Some(BranchType::Local))? {
        let (branch, _) = branch?;
        let branch_name = branch.name()?.context("non-utf8 branch name")?;
        debug!("Branch: {:?}", branch.name()?);
        if config::get_remote(config, branch_name)?.is_implicit() {
            debug!(
                "Skip: the branch doesn't have a tracking remote: {:?}",
                branch_name
            );
            continue;
        }
        if let Some(remote_ref) = get_fetch_remote_ref(repo, config, branch_name)? {
            if Some(&remote_ref) == Some(&base_remote_ref) {
                debug!("Skip: the branch is the base: {:?}", branch_name);
                continue;
            }
        }
        let reference = branch.get();
        if reference.symbolic_target().is_some() {
            debug!("Skip: the branch is a symbolic ref: {:?}", branch_name);
            continue;
        }
        let merged = merged_locals.contains(branch_name)
            || subprocess::is_merged(repo, &base_remote_ref, branch_name)?;
        let fetch = get_fetch_remote_ref(repo, config, branch_name)?;
        let push = get_push_remote_ref(repo, config, branch_name)?;
        trace!("merged: {}", merged);
        trace!("fetch: {:?}", fetch);
        trace!("push: {:?}", push);
        match (fetch, push) {
            (Some(_), Some(remote_ref)) if merged => {
                debug!("merged local, merged remote: the branch is merged, but forgot to delete");
                result.merged_locals.insert(branch_name.to_string());
                result.merged_remotes.insert(remote_ref);
            }
            (Some(_), Some(_)) => {
                debug!("skip: live branch. not merged, not gone");
            }

            // `git branch`'s shows `%(upstream)` as s `%(push)` fallback if there isn't a specified push remote.
            // But our `get_push_remote_ref` doesn't.
            (Some(fetch_ref), None) if merged => {
                debug!("merged local, merged remote: the branch is merged, but forgot to delete");
                // TODO: it might be a long running branch like 'develop' in a git-flow
                result.merged_locals.insert(branch_name.to_string());
                result.merged_remotes.insert(fetch_ref);
            }
            (Some(_), None) => {
                debug!("skip: it might be a long running branch like 'develop' in a git-flow");
            }

            (None, Some(remote_ref)) if merged => {
                debug!("merged remote: it might be a long running branch like 'develop' which is once pushed to the personal repo in the triangular workflow, but the branch is merged on the upstream");
                result.merged_remotes.insert(remote_ref);
            }
            (None, Some(remote_ref)) => {
                debug!("gone remote: it might be a long running branch like 'develop' which is once pushed to the personal repo in the triangular workflow, but the branch is gone on the upstream");
                result.gone_remotes.insert(remote_ref);
            }

            (None, None) if merged => {
                debug!("merged local: the branch is merged, and deleted");
                result.merged_locals.insert(branch_name.to_string());
            }
            (None, None) => {
                debug!("gone local: the branch is not merged but gone somehow");
                result.gone_locals.insert(branch_name.to_string());
            }
        }
    }

    Ok(result)
}

fn resolve_config_base_ref(repo: &Repository, config: &Config, base: &str) -> Result<String> {
    // find "master -> refs/remotes/origin/master"
    if let Some(remote_ref) = get_fetch_remote_ref(repo, config, base)? {
        trace!("Found fetch remote ref for: {}, {}", base, remote_ref);
        return Ok(remote_ref);
    }

    // match "origin/master -> refs/remotes/origin/master"
    if let Ok(remote_ref) = repo.find_reference(&format!("refs/remotes/{}", base)) {
        let refname = remote_ref.name().context("non-utf8 reference name")?;
        trace!("Found remote ref for: {}, {}", base, refname);
        return Ok(refname.to_string());
    }

    trace!("Not found remote refs. fallback: {}", base);
    Ok(repo
        .find_reference(base)?
        .name()
        .context("non-utf8 ref")?
        .to_string())
}

pub fn delete_local_branches(repo: &Repository, branches: &[&str], dry_run: bool) -> Result<()> {
    if branches.is_empty() {
        return Ok(());
    }

    let detach_to = if repo.head_detached()? {
        None
    } else {
        let head = repo.head()?;
        let head_refname = head.name().context("non-utf8 head ref name")?;
        assert!(head_refname.starts_with("refs/heads/"));
        let head_name = &head_refname["refs/heads/".len()..];
        if branches.contains(&head_name) {
            Some(head)
        } else {
            None
        }
    };

    if let Some(head) = detach_to {
        subprocess::checkout(repo, head, dry_run)?;
    }
    subprocess::branch_delete(repo, branches, dry_run)?;

    Ok(())
}

pub fn delete_remote_branches(
    repo: &Repository,
    remote_refs: &[&str],
    dry_run: bool,
) -> Result<()> {
    if remote_refs.is_empty() {
        return Ok(());
    }
    let mut per_remote = HashMap::new();
    for remote_ref in remote_refs {
        let (remote_name, ref_on_remote) = get_remote_name_and_ref_on_remote(repo, remote_ref)?;
        let entry = per_remote.entry(remote_name).or_insert_with(Vec::new);
        entry.push(ref_on_remote);
    }
    for (remote_name, remote_refnames) in per_remote.iter() {
        subprocess::push_delete(repo, remote_name, remote_refnames, dry_run)?;
    }
    Ok(())
}

fn get_remote_name_and_ref_on_remote(
    repo: &Repository,
    remote_ref: &str,
) -> Result<(String, String)> {
    assert!(remote_ref.starts_with("refs/remotes/"));
    for remote_name in repo.remotes()?.iter() {
        let remote_name = remote_name.context("non-utf8 remote name")?;
        let remote = repo.find_remote(&remote_name)?;
        if let Some(expanded) =
            expand_refspec(&remote, remote_ref, Direction::Fetch, ExpansionSide::Left)?
        {
            return Ok((
                remote.name().context("non-utf8 remote name")?.to_string(),
                expanded,
            ));
        }
    }
    unreachable!("matching refspec is not found");
}
