pub mod args;
pub mod config;
mod remote_ref;
mod simple_glob;
mod subprocess;

use std::collections::{HashMap, HashSet};
use std::convert::TryFrom;
use std::ops::Deref;

use anyhow::{Context, Result};
use git2::{BranchType, Config as GitConfig, Error as GitError, ErrorCode, Repository};
use glob::Pattern;
use log::*;
use rayon::prelude::*;

use crate::args::{Category, DeleteFilter};
use crate::remote_ref::{
    get_fetch_remote_ref, get_push_remote_ref, get_ref_on_remote_from_remote_ref,
};
pub use crate::subprocess::remote_update;

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
}

impl MergedOrGone {
    pub fn accumulate(mut self, mut other: Self) -> Self {
        self.merged_locals.extend(other.merged_locals.drain());
        self.gone_locals.extend(other.gone_locals.drain());
        self.merged_remotes.extend(other.merged_remotes.drain());
        self.gone_remotes.extend(other.gone_remotes.drain());

        self
    }
}

#[derive(Default, Eq, PartialEq, Debug)]
pub struct MergedOrGoneAndKeptBacks {
    pub to_delete: MergedOrGone,
    pub kept_back: HashMap<String, String>,
}

impl MergedOrGoneAndKeptBacks {
    fn keep_base(&mut self, repo: &Repository, config: &GitConfig, bases: &[&str]) -> Result<()> {
        let base_refs = resolve_base_refs(repo, config, bases)?;
        trace!("base_refs: {:#?}", base_refs);
        self.kept_back.extend(keep_branches(
            repo,
            &base_refs,
            "Merged local but kept back because it is a base",
            &mut self.to_delete.merged_locals,
        )?);
        self.kept_back.extend(keep_branches(
            repo,
            &base_refs,
            "Gone local but kept back because it is a base",
            &mut self.to_delete.gone_locals,
        )?);
        self.kept_back.extend(keep_remote_refs(
            &base_refs,
            "Merged remotes but kept back because it is a base",
            &mut self.to_delete.merged_remotes,
        ));
        self.kept_back.extend(keep_remote_refs(
            &base_refs,
            "Gone remotes but kept back because it is a base",
            &mut self.to_delete.gone_remotes,
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
            &mut self.to_delete.merged_locals,
        )?);
        self.kept_back.extend(keep_branches(
            repo,
            &protected_refs,
            "Gone local but kept back because it is protected",
            &mut self.to_delete.gone_locals,
        )?);
        self.kept_back.extend(keep_remote_refs(
            &protected_refs,
            "Merged remotes but kept back because it is protected",
            &mut self.to_delete.merged_remotes,
        ));
        self.kept_back.extend(keep_remote_refs(
            &protected_refs,
            "Gone remotes but kept back because it is protected",
            &mut self.to_delete.gone_remotes,
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

        if self.to_delete.merged_locals.contains(head_name) {
            self.to_delete.merged_locals.remove(head_name);
            self.kept_back.insert(
                head_name.to_string(),
                "Merged local but kept back not to make detached HEAD".to_string(),
            );
        }
        if self.to_delete.gone_locals.contains(head_name) {
            self.to_delete.gone_locals.remove(head_name);
            self.kept_back.insert(
                head_name.to_string(),
                "Gone local but kept back not to make detached HEAD".to_string(),
            );
        }
        Ok(())
    }

    pub fn print_summary(&self, repo: &Repository, filter: &DeleteFilter) -> Result<()> {
        fn print(branches: &HashSet<String>, filter: &DeleteFilter, category: Category) {
            if branches.is_empty() {
                return;
            }
            let mut branches: Vec<_> = branches.iter().collect();
            branches.sort();
            if filter.contains(&category) {
                println!("Delete {}:", category);
                for branch in branches {
                    println!("  - {}", branch);
                }
                println!();
            } else {
                println!("Skip {}:", category);
                for branch in branches {
                    println!("    {}", branch);
                }
                println!();
            }
        }
        print(&self.to_delete.merged_locals, filter, Category::MergedLocal);
        print(
            &self.to_delete.merged_remotes,
            filter,
            Category::MergedRemote,
        );

        print(&self.to_delete.gone_locals, filter, Category::GoneLocal);
        print(&self.to_delete.gone_remotes, filter, Category::GoneRemote);

        if !self.kept_back.is_empty() {
            let mut kept_back: Vec<_> = self.kept_back.iter().collect();
            kept_back.sort();
            println!("Kept back:");
            for (branch, reason) in kept_back {
                println!("    {}\t{}", branch, reason);
            }
            println!();
        }

        println!("Branches that will remain:");
        println!("  local branches:");
        let local_branches_to_delete: HashSet<_> = self
            .get_local_branches_to_delete(filter)
            .into_iter()
            .collect();
        for local_branch in repo.branches(Some(BranchType::Local))? {
            let (branch, _) = local_branch?;
            let name = branch.name()?.context("non utf-8 local branch name")?;
            if local_branches_to_delete.contains(name) {
                continue;
            }
            println!("    {}", name);
        }
        println!("  remote references:");
        let remote_refs_to_delete: HashSet<_> =
            self.get_remote_refs_to_delete(filter).into_iter().collect();
        for remote_ref in repo.branches(Some(BranchType::Remote))? {
            let (branch, _) = remote_ref?;
            let name = branch.get().name().context("non utf-8 remote ref name")?;
            if remote_refs_to_delete.contains(name) {
                continue;
            }
            println!("    {}", name);
        }
        println!();
        Ok(())
    }

    pub fn get_local_branches_to_delete(&self, filter: &DeleteFilter) -> Vec<&str> {
        let mut result = Vec::new();
        if filter.contains(&Category::MergedLocal) {
            result.extend(self.to_delete.merged_locals.iter().map(String::as_str))
        }
        if filter.contains(&Category::GoneLocal) {
            result.extend(self.to_delete.gone_locals.iter().map(String::as_str))
        }
        result
    }

    pub fn get_remote_refs_to_delete(&self, filter: &DeleteFilter) -> Vec<&str> {
        let mut result = Vec::new();
        if filter.contains(&Category::MergedRemote) {
            result.extend(self.to_delete.merged_remotes.iter().map(String::as_str))
        }
        if filter.contains(&Category::GoneLocal) {
            result.extend(self.to_delete.gone_remotes.iter().map(String::as_str))
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
pub fn get_merged_or_gone(git: &Git, config: &Config) -> Result<MergedOrGoneAndKeptBacks> {
    let base_remote_refs = resolve_base_remote_refs(&git.repo, &git.config, &config.bases)?;
    trace!("base_remote_refs: {:#?}", base_remote_refs);

    let protected_refs =
        resolve_protected_refs(&git.repo, &git.config, &config.protected_branches)?;
    trace!("protected_refs: {:#?}", protected_refs);

    let mut merged_or_gone = MergedOrGone::default();
    // Fast filling ff merged branches
    let noff_merged_locals =
        subprocess::get_noff_merged_locals(&git.repo, &git.config, &base_remote_refs)?;
    merged_or_gone
        .merged_locals
        .extend(noff_merged_locals.clone());

    let mut merged_locals = HashSet::new();
    merged_locals.extend(noff_merged_locals);

    let mut base_and_branch_to_compare = Vec::new();
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
        for base_remote_ref in &base_remote_refs {
            base_and_branch_to_compare.push((base_remote_ref.to_string(), branch_name.to_string()));
        }
    }

    let classifications = base_and_branch_to_compare
        .into_par_iter()
        .map({
            // git's fields are semantically Send + Sync in the `classify`.
            // They are read only in `classify` function.
            // It is denoted that it is safe in that case
            // https://github.com/libgit2/libgit2/blob/master/docs/threading.md#sharing-objects
            let git = ForceSendSync(git);
            move |(base_remote_ref, branch_name)| {
                classify(git, &merged_locals, &base_remote_ref, &branch_name).with_context(|| {
                    format!(
                        "base_remote_ref={}, branch_name={}",
                        base_remote_ref, branch_name
                    )
                })
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    for classification in classifications.into_iter() {
        debug!("branch: {}", classification.branch_name);
        trace!("merged: {}", classification.branch_is_merged);
        trace!("push: {:?}", classification.fetch);
        trace!("fetch: {:?}", classification.push);
        debug!("message: {}", classification.message);
        merged_or_gone = merged_or_gone.accumulate(classification.result);
    }

    let mut result = MergedOrGoneAndKeptBacks {
        to_delete: merged_or_gone,
        kept_back: HashMap::new(),
    };
    result.keep_base(&git.repo, &git.config, &config.bases)?;
    result.keep_protected(&git.repo, &git.config, &config.protected_branches)?;

    if !config.detach {
        result.adjust_not_to_detach(&git.repo)?;
    }

    Ok(result)
}

struct Classification {
    branch_name: String,
    branch_is_merged: bool,
    fetch: Option<String>,
    push: Option<String>,
    message: &'static str,
    result: MergedOrGone,
}

/// Make sure repo and config are semantically Send + Sync.
fn classify(
    git: ForceSendSync<&Git>,
    merged_locals: &HashSet<String>,
    base_remote_ref: &str,
    branch_name: &str,
) -> Result<Classification> {
    let merged = merged_locals.contains(branch_name)
        || subprocess::is_merged(&git.repo, base_remote_ref, branch_name)?;
    let fetch = get_fetch_remote_ref(&git.repo, &git.config, branch_name)?;
    let push = get_push_remote_ref(&git.repo, &git.config, branch_name)?;

    let mut c = Classification {
        branch_name: branch_name.to_string(),
        branch_is_merged: merged,
        fetch: fetch.clone(),
        push: push.clone(),
        message: "",
        result: MergedOrGone::default(),
    };

    match (fetch, push) {
        (Some(_), Some(remote_ref)) if merged => {
            c.message = "merged local, merged remote: the branch is merged, but forgot to delete";
            c.result.merged_locals.insert(branch_name.to_string());
            c.result.merged_remotes.insert(remote_ref);
        }
        (Some(_), Some(_)) => {
            c.message = "skip: live branch. not merged, not gone";
        }

        // `git branch`'s shows `%(upstream)` as s `%(push)` fallback if there isn't a specified push remote.
        // But our `get_push_remote_ref` doesn't.
        (Some(fetch_ref), None) if merged => {
            c.message = "merged local, merged remote: the branch is merged, but forgot to delete";
            c.result.merged_locals.insert(branch_name.to_string());
            c.result.merged_remotes.insert(fetch_ref);
        }
        (Some(_), None) => {
            c.message = "skip: it might be a long running branch like 'develop' in a git-flow";
        }

        (None, Some(remote_ref)) if merged => {
            c.message = "merged remote: it might be a long running branch like 'develop' which is once pushed to the personal git.repo in the triangular workflow, but the branch is merged on the upstream";
            c.result.merged_remotes.insert(remote_ref);
        }
        (None, Some(remote_ref)) => {
            c.message = "gone remote: it might be a long running branch like 'develop' which is once pushed to the personal git.repo in the triangular workflow, but the branch is gone on the upstream";
            c.result.gone_remotes.insert(remote_ref);
        }

        (None, None) if merged => {
            c.message = "merged local: the branch is merged, and deleted";
            c.result.merged_locals.insert(branch_name.to_string());
        }
        (None, None) => {
            c.message = "gone local: the branch is not merged but gone somehow";
            c.result.gone_locals.insert(branch_name.to_string());
        }
    }

    Ok(c)
}

/// Use with caution.
/// It makes wrapping type T to be Send + Sync.
/// Make sure T is semantically Send + Sync
#[derive(Copy, Clone)]
struct ForceSendSync<T>(T);

unsafe impl<T> Sync for ForceSendSync<T> {}
unsafe impl<T> Send for ForceSendSync<T> {}

impl<T> Deref for ForceSendSync<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
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
        let ref_on_remote = get_ref_on_remote_from_remote_ref(repo, remote_ref)?;
        let entry = per_remote
            .entry(ref_on_remote.remote_name)
            .or_insert_with(Vec::new);
        entry.push(ref_on_remote.refname);
    }
    for (remote_name, remote_refnames) in per_remote.iter() {
        subprocess::push_delete(repo, remote_name, remote_refnames, dry_run)?;
    }
    Ok(())
}
