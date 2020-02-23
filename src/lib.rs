pub mod args;
pub mod config;
mod remote_ref;
mod simple_glob;
mod subprocess;

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use git2::{BranchType, Config as GitConfig, Direction, Error as GitError, ErrorCode, Repository};
use glob::Pattern;
use log::*;

use crate::args::{Category, DeleteFilter};
use crate::remote_ref::{get_fetch_remote_ref, get_push_remote_ref};
use crate::simple_glob::{expand_refspec, ExpansionSide};
pub use crate::subprocess::remote_update;
use std::convert::TryFrom;

pub struct Git {
    pub repo: Repository,
    pub config: GitConfig,
}

impl TryFrom<Repository> for Git {
    type Error = GitError;

    fn try_from(repo: Repository) -> Result<Self, Self::Error> {
        let config = repo.config()?.snapshot()?;
        Ok(Self { repo, config })
    }
}

pub struct Config<'a> {
    pub bases: Vec<&'a str>,
    pub protected_branches: HashSet<&'a str>,
    pub detach: bool,
}

#[derive(Default, Eq, PartialEq, Debug)]
pub struct MergedOrGone {
    // local branches
    pub merged_locals: HashSet<String>,
    pub gone_locals: HashSet<String>,

    /// remote refs
    pub merged_remotes: HashSet<String>,
    pub gone_remotes: HashSet<String>,

    pub kept_back: HashMap<String, String>,
}

impl MergedOrGone {
    fn keep_base(&mut self, repo: &Repository, config: &GitConfig, bases: &[&str]) -> Result<()> {
        let base_refs = resolve_base_refs(repo, config, bases)?;
        trace!("base_refs: {:#?}", base_refs);
        self.kept_back.extend(keep_branches(
            repo,
            &base_refs,
            "Merged local but kept back because it is a base",
            &mut self.merged_locals,
        )?);
        self.kept_back.extend(keep_branches(
            repo,
            &base_refs,
            "Gone local but kept back because it is a base",
            &mut self.gone_locals,
        )?);
        self.kept_back.extend(keep_remote_refs(
            &base_refs,
            "Merged remotes but kept back because it is a base",
            &mut self.merged_remotes,
        ));
        self.kept_back.extend(keep_remote_refs(
            &base_refs,
            "Gone remotes but kept back because it is a base",
            &mut self.gone_remotes,
        ));
        Ok(())
    }

    fn keep_protected(
        &mut self,
        repo: &Repository,
        config: &GitConfig,
        protected_branches: &HashSet<&str>,
    ) -> Result<()> {
        let protected_refs = resolve_protected_refs(repo, config, protected_branches)?;
        trace!("protected_refs: {:#?}", protected_refs);
        self.kept_back.extend(keep_branches(
            repo,
            &protected_refs,
            "Merged local but kept back because it is protected",
            &mut self.merged_locals,
        )?);
        self.kept_back.extend(keep_branches(
            repo,
            &protected_refs,
            "Gone local but kept back because it is protected",
            &mut self.gone_locals,
        )?);
        self.kept_back.extend(keep_remote_refs(
            &protected_refs,
            "Merged remotes but kept back because it is protected",
            &mut self.merged_remotes,
        ));
        self.kept_back.extend(keep_remote_refs(
            &protected_refs,
            "Gone remotes but kept back because it is protected",
            &mut self.gone_remotes,
        ));
        Ok(())
    }

    fn adjust_not_to_detach(&mut self, repo: &Repository) -> Result<()> {
        if repo.head_detached()? {
            return Ok(());
        }
        let head = repo.head()?;
        let head_name = head.name().context("non-utf8 head ref name")?;
        assert!(head_name.starts_with("refs/heads/"));
        let head_name = &head_name["refs/heads/".len()..];

        if self.merged_locals.contains(head_name) {
            self.merged_locals.remove(head_name);
            self.kept_back.insert(
                head_name.to_string(),
                "Merged local but kept back not to make detached HEAD".to_string(),
            );
        }
        if self.gone_locals.contains(head_name) {
            self.gone_locals.remove(head_name);
            self.kept_back.insert(
                head_name.to_string(),
                "Gone local but kept back not to make detached HEAD".to_string(),
            );
        }
        Ok(())
    }

    pub fn print_summary(&self, filter: &DeleteFilter) {
        fn print(branches: &HashSet<String>, filter: &DeleteFilter, category: Category) {
            if branches.is_empty() {
                return;
            }
            let mut branches: Vec<_> = branches.iter().collect();
            branches.sort();
            if filter.contains(&category) {
                println!("Delete {}:", category);
                for branch in branches {
                    println!("  {}", branch);
                }
            } else {
                println!("Skip {}:", category);
                for branch in branches {
                    println!("  {}", branch);
                }
            }
        }
        print(&self.merged_locals, filter, Category::MergedLocal);
        print(&self.merged_remotes, filter, Category::MergedRemote);

        print(&self.gone_locals, filter, Category::GoneLocal);
        print(&self.gone_remotes, filter, Category::GoneRemote);

        if !self.kept_back.is_empty() {
            let mut kept_back: Vec<_> = self.kept_back.iter().collect();
            kept_back.sort();
            println!("Kept back:");
            for (branch, reason) in kept_back {
                println!("  {}\t{}", branch, reason);
            }
        }
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

fn keep_branches(
    repo: &Repository,
    protected_refs: &HashSet<String>,
    reason: &str,
    branches: &mut HashSet<String>,
) -> Result<HashMap<String, String>> {
    let mut kept_back = HashMap::new();
    let mut bag = HashSet::new();
    for branch_name in branches.iter() {
        let branch = repo.find_branch(branch_name, BranchType::Local)?;
        let reference = branch.into_reference();
        let refname = reference.name().context("non utf-8 branch ref")?;
        if protected_refs.contains(branch_name) {
            bag.insert(branch_name.to_string());
            bag.insert(refname.to_string());
            kept_back.insert(branch_name.to_string(), reason.to_string());
        } else if protected_refs.contains(refname) {
            bag.insert(branch_name.to_string());
            kept_back.insert(refname.to_string(), reason.to_string());
        }
    }
    for branch in bag.into_iter() {
        branches.remove(&branch);
    }
    Ok(kept_back)
}

fn keep_remote_refs(
    protected_refs: &HashSet<String>,
    reason: &str,
    remote_refs: &mut HashSet<String>,
) -> HashMap<String, String> {
    let mut kept_back = HashMap::new();
    for remote_ref in remote_refs.iter() {
        if protected_refs.contains(remote_ref) {
            kept_back.insert(remote_ref.to_string(), reason.to_string());
        }
    }
    for remote_ref in kept_back.keys() {
        remote_refs.remove(remote_ref);
    }
    kept_back
}

#[allow(clippy::cognitive_complexity, clippy::implicit_hasher)]
pub fn get_merged_or_gone(git: &Git, config: &Config) -> Result<MergedOrGone> {
    let base_remote_refs = resolve_base_remote_refs(&git.repo, &git.config, &config.bases)?;
    trace!("base_remote_refs: {:#?}", base_remote_refs);

    let protected_refs =
        resolve_protected_refs(&git.repo, &git.config, &config.protected_branches)?;
    trace!("protected_refs: {:#?}", protected_refs);

    let mut result = MergedOrGone::default();
    // Fast filling ff merged branches
    let noff_merged_locals =
        subprocess::get_noff_merged_locals(&git.repo, &git.config, &base_remote_refs)?;
    result.merged_locals.extend(noff_merged_locals.clone());

    let mut merged_locals = HashSet::new();
    merged_locals.extend(noff_merged_locals);

    for branch in git.repo.branches(Some(BranchType::Local))? {
        let (branch, _) = branch?;
        let branch_name = branch.name()?.context("non-utf8 branch name")?;
        debug!("Branch: {:?}", branch.name()?);
        if config::get_remote(&git.config, branch_name)?.is_implicit() {
            debug!(
                "Skip: the branch doesn't have a tracking remote: {:?}",
                branch_name
            );
            continue;
        }
        if protected_refs.contains(branch_name) {
            debug!("Skip: the branch is protected branch: {:?}", branch_name);
            continue;
        }
        if let Some(remote_ref) = get_fetch_remote_ref(&git.repo, &git.config, branch_name)? {
            if base_remote_refs.contains(&remote_ref) {
                debug!("Skip: the branch is the base: {:?}", branch_name);
                continue;
            }
            if protected_refs.contains(&remote_ref) {
                debug!(
                    "Skip: the branch tracks protected branch: {:?}",
                    branch_name
                );
            }
        }
        let reference = branch.get();
        if reference.symbolic_target().is_some() {
            debug!("Skip: the branch is a symbolic ref: {:?}", branch_name);
            continue;
        }
        let merged = merged_locals.contains(branch_name)
            || subprocess::is_merged(&git.repo, &base_remote_refs, branch_name)?;
        let fetch = get_fetch_remote_ref(&git.repo, &git.config, branch_name)?;
        let push = get_push_remote_ref(&git.repo, &git.config, branch_name)?;
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
                debug!("merged remote: it might be a long running branch like 'develop' which is once pushed to the personal git.repo in the triangular workflow, but the branch is merged on the upstream");
                result.merged_remotes.insert(remote_ref);
            }
            (None, Some(remote_ref)) => {
                debug!("gone remote: it might be a long running branch like 'develop' which is once pushed to the personal git.repo in the triangular workflow, but the branch is gone on the upstream");
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

    result.keep_base(&git.repo, &git.config, &config.bases)?;
    result.keep_protected(&git.repo, &git.config, &config.protected_branches)?;

    if !config.detach {
        result.adjust_not_to_detach(&git.repo)?;
    }

    Ok(result)
}

/// if there are following references:
/// refs/heads/master
/// refs/remotes/origin/master
/// refs/remotes/upstream/master
/// and master's upstreams:
/// fetch: upstream/release-v1.x
/// push: origin/release-v1.x
///
/// master
/// refs/heads/master because it shouldn't be removed from the local
/// refs/remotes/origin/master because it shouldn't be removed from the push remote
/// refs/remotes/upstream/master because it shouldn't be remvoed from the fetch remote
fn resolve_base_refs(
    repo: &Repository,
    config: &GitConfig,
    bases: &[&str],
) -> Result<HashSet<String>> {
    let mut result = HashSet::new();
    for base in bases {
        match repo.find_branch(base, BranchType::Local) {
            Ok(branch) => {
                let refname = branch.get().name().context("non utf-8 base branch ref")?;
                result.insert((*base).to_string());
                result.insert((*refname).to_string());
            }
            Err(err) if err.code() == ErrorCode::NotFound => continue,
            Err(err) => return Err(err.into()),
        }

        if let Some(remote_ref) = get_fetch_remote_ref(repo, config, base)? {
            result.insert(remote_ref);
        }

        if let Some(remote_ref) = get_push_remote_ref(repo, config, base)? {
            result.insert(remote_ref);
        }
    }
    Ok(result)
}

fn resolve_base_remote_refs(
    repo: &Repository,
    config: &GitConfig,
    bases: &[&str],
) -> Result<Vec<String>> {
    let mut result = Vec::new();
    for base in bases {
        // find "master -> refs/remotes/origin/master"
        if let Some(remote_ref) = get_fetch_remote_ref(repo, config, base)? {
            result.push(remote_ref);
            continue;
        }

        // match "origin/master -> refs/remotes/origin/master"
        if let Ok(remote_ref) = repo.find_reference(&format!("refs/remotes/{}", base)) {
            let refname = remote_ref.name().context("non-utf8 reference name")?;
            result.push(refname.to_string());
            continue;
        }

        if base.starts_with("refs/remotes/") {
            result.push((*base).to_string());
            continue;
        }
    }
    Ok(result)
}

/// protected branch patterns
/// if there are following references:
/// refs/heads/release-v1.x
/// refs/remotes/origin/release-v1.x
/// refs/remotes/upstream/release-v1.x
/// and release-v1.x tracks upstream/release-v1.x
///
/// release-*
/// -> refs/heads/release-v1.x,
///    refs/remotes/upstream/release-v1.x,
/// origin/release-*
/// -> refs/remotes/origin/release-v1.x
/// refs/heads/release-*
/// -> refs/heads/release-v1.x
/// refs/remotes/origin/release-*
/// -> refs/remotes/origin/release-v1.x
#[allow(clippy::implicit_hasher)]
fn resolve_protected_refs(
    repo: &Repository,
    config: &GitConfig,
    protected_branches: &HashSet<&str>,
) -> Result<HashSet<String>> {
    let mut result = HashSet::default();
    for protected in protected_branches {
        for reference in repo.references_glob(protected)? {
            let reference = reference?;
            let refname = reference.name().context("non utf-8 refname")?;
            result.insert(refname.to_string());
        }
        for reference in repo.references_glob(&format!("refs/remotes/{}", protected))? {
            let reference = reference?;
            let refname = reference.name().context("non utf-8 refname")?;
            result.insert(refname.to_string());
        }
        for branch in repo.branches(Some(BranchType::Local))? {
            let (branch, _) = branch?;
            let branch_name = branch.name()?.context("non utf-8 branch name")?;
            if Pattern::new(protected)?.matches(branch_name) {
                result.insert(branch_name.to_string());
                if let Some(remote_ref) = get_fetch_remote_ref(repo, config, branch_name)? {
                    result.insert(remote_ref);
                }
                let reference = branch.into_reference();
                let refname = reference.name().context("non utf-8 ref")?;
                result.insert(refname.to_string());
            }
        }
    }
    Ok(result)
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
