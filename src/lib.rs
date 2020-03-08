pub mod args;
mod branch;
pub mod config;
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

use crate::args::DeleteFilter;
use crate::branch::{
    get_fetch_upstream, get_push_upstream, get_remote, get_remote_branch_from_ref,
};
use crate::subprocess::ls_remote_heads;
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
    pub filter: DeleteFilter,
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
    fn accumulate(mut self, mut other: Self) -> Self {
        self.merged_locals.extend(other.merged_locals.drain());
        self.gone_locals.extend(other.gone_locals.drain());
        self.merged_remotes.extend(other.merged_remotes.drain());
        self.gone_remotes.extend(other.gone_remotes.drain());

        self
    }

    pub fn locals(&self) -> Vec<&str> {
        self.merged_locals
            .iter()
            .chain(self.gone_locals.iter())
            .map(String::as_str)
            .collect()
    }

    pub fn remotes(&self) -> Vec<&str> {
        self.merged_remotes
            .iter()
            .chain(self.gone_remotes.iter())
            .map(String::as_str)
            .collect()
    }

    fn apply_filter(&mut self, repo: &Repository, filter: &DeleteFilter) -> Result<()> {
        trace!("Before filter: {:#?}", self);
        trace!("Applying filter: {:?}", filter);
        if !filter.filter_merged_local() {
            trace!("filter-out: merged local branches {:?}", self.merged_locals);
            self.merged_locals.clear();
        }
        if !filter.filter_gone_local() {
            trace!("filter-out: gone local branches {:?}", self.merged_locals);
            self.gone_locals.clear();
        }

        let mut merged_remotes = HashSet::new();
        for remote_ref in &self.merged_remotes {
            let remote_branch = get_remote_branch_from_ref(repo, remote_ref)?;
            if filter.filter_merged_remote(&remote_branch.remote_name) {
                merged_remotes.insert(remote_ref.clone());
            } else {
                trace!("filter-out: merged remote ref {}", remote_ref);
            }
        }
        self.merged_remotes = merged_remotes;

        let mut gone_remotes = HashSet::new();
        for remote_ref in &self.gone_remotes {
            let ref_on_remote = get_remote_branch_from_ref(repo, remote_ref)?;
            if filter.filter_gone_remote(&ref_on_remote.remote_name) {
                gone_remotes.insert(remote_ref.clone());
            } else {
                trace!("filter-out: gone_remotes remote ref {}", remote_ref);
            }
        }
        self.gone_remotes = gone_remotes;

        Ok(())
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
    let base_upstreams = resolve_base_upstream(&git.repo, &git.config, &config.bases)?;
    trace!("base_upstreams: {:#?}", base_upstreams);

    let protected_refs =
        resolve_protected_refs(&git.repo, &git.config, &config.protected_branches)?;
    trace!("protected_refs: {:#?}", protected_refs);

    let mut merged_or_gone = MergedOrGone::default();
    // Fast filling ff merged branches
    let noff_merged_locals =
        subprocess::get_noff_merged_locals(&git.repo, &git.config, &base_upstreams)?;
    merged_or_gone
        .merged_locals
        .extend(noff_merged_locals.clone());

    let mut merged_locals = HashSet::new();
    merged_locals.extend(noff_merged_locals);

    let mut base_and_branch_to_compare = Vec::new();
    let mut remote_urls = Vec::new();
    for branch in git.repo.branches(Some(BranchType::Local))? {
        let (branch, _) = branch?;
        let branch_name = branch.name()?.context("non-utf8 branch name")?;
        debug!("Branch: {:?}", branch.name()?);
        let config_remote = config::get_remote(&git.config, branch_name)?;
        if config_remote.is_implicit() {
            debug!(
                "Skip: the branch doesn't have a tracking remote: {:?}",
                branch_name
            );
            continue;
        }
        if get_remote(&git.repo, &config_remote)?.is_none() {
            debug!(
                "The branch's remote is assumed to be an URL: {}",
                config_remote.as_str()
            );
            remote_urls.push(config_remote.to_string());
        }

        if protected_refs.contains(branch_name) {
            debug!("Skip: the branch is protected branch: {:?}", branch_name);
            continue;
        }
        if let Some(upstream) = get_fetch_upstream(&git.repo, &git.config, branch_name)? {
            if base_upstreams.contains(&upstream) {
                debug!("Skip: the branch is the base: {:?}", branch_name);
                continue;
            }
            if protected_refs.contains(&upstream) {
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
        for base_upstream in &base_upstreams {
            base_and_branch_to_compare.push((base_upstream.to_string(), branch_name.to_string()));
        }
    }

    let remote_heads_per_url = remote_urls
        .into_par_iter()
        .map({
            let git = ForceSendSync(git);
            move |remote_url| {
                ls_remote_heads(&git.repo, &remote_url)
                    .with_context(|| format!("remote_url={}", remote_url))
                    .map(|remote_heads| (remote_url.to_string(), remote_heads))
            }
        })
        .collect::<Result<HashMap<String, HashSet<String>>, _>>()?;

    let classifications = base_and_branch_to_compare
        .into_par_iter()
        .map({
            // git's fields are semantically Send + Sync in the `classify`.
            // They are read only in `classify` function.
            // It is denoted that it is safe in that case
            // https://github.com/libgit2/libgit2/blob/master/docs/threading.md#sharing-objects
            let git = ForceSendSync(git);
            move |(base_upstream, branch_name)| {
                classify(
                    git,
                    &merged_locals,
                    &remote_heads_per_url,
                    &base_upstream,
                    &branch_name,
                )
                .with_context(|| {
                    format!(
                        "base_upstream={}, branch_name={}",
                        base_upstream, branch_name
                    )
                })
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    for classification in classifications.into_iter() {
        debug!("branch: {}", classification.branch_name);
        trace!("merged: {}", classification.branch_is_merged);
        trace!("fetch: {:?}", classification.fetch);
        trace!("push: {:?}", classification.push);
        debug!("message: {}", classification.message);
        merged_or_gone = merged_or_gone.accumulate(classification.result);
    }
    merged_or_gone.apply_filter(&git.repo, &config.filter)?;

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
    remote_heads_per_url: &HashMap<String, HashSet<String>>,
    base_upstream: &str,
    branch_name: &str,
) -> Result<Classification> {
    let merged = merged_locals.contains(branch_name)
        || subprocess::is_merged(&git.repo, base_upstream, branch_name)?;
    let fetch = get_fetch_upstream(&git.repo, &git.config, branch_name)?;
    let push = get_push_upstream(&git.repo, &git.config, branch_name)?;

    let mut c = Classification {
        branch_name: branch_name.to_string(),
        branch_is_merged: merged,
        fetch: fetch.clone(),
        push: push.clone(),
        message: "",
        result: MergedOrGone::default(),
    };

    match (fetch, push) {
        (Some(_), Some(upstream)) if merged => {
            c.message = "merged local, merged remote: the branch is merged, but forgot to delete";
            c.result.merged_locals.insert(branch_name.to_string());
            c.result.merged_remotes.insert(upstream);
        }
        (Some(_), Some(_)) => {
            c.message = "skip: live branch. not merged, not gone";
        }

        // `git branch`'s shows `%(upstream)` as s `%(push)` fallback if there isn't a specified push remote.
        // But our `get_push_remote_ref` doesn't.
        (Some(upstream), None) if merged => {
            c.message = "merged local, merged remote: the branch is merged, but forgot to delete";
            c.result.merged_locals.insert(branch_name.to_string());
            c.result.merged_remotes.insert(upstream);
        }
        (Some(_), None) => {
            c.message = "skip: it might be a long running branch like 'develop' in a git-flow";
        }

        (None, Some(upstream)) if merged => {
            c.message = "merged remote: it might be a long running branch like 'develop' which is once pushed to the personal git.repo in the triangular workflow, but the branch is merged on the upstream";
            c.result.merged_remotes.insert(upstream);
        }
        (None, Some(upstream)) => {
            c.message = "gone remote: it might be a long running branch like 'develop' which is once pushed to the personal git.repo in the triangular workflow, but the branch is gone on the upstream";
            c.result.gone_remotes.insert(upstream);
        }

        (None, None) if merged => {
            c.message = "merged local: the branch is merged, and deleted";
            c.result.merged_locals.insert(branch_name.to_string());
        }
        (None, None) => {
            // `origin` or `git@github.com:someone/fork.git`
            let remote = config::get_remote_raw(&git.config, branch_name)?
                .expect("should have it if it has an upstream");
            let merge = config::get_merge(&git.config, branch_name)?
                .expect("should have it if it has an upstream");
            if remote_heads_per_url.contains_key(&remote)
                && remote_heads_per_url[&remote].contains(&merge)
            {
                c.message = "skip: the branch is alive";
            } else {
                c.message = "gone local: the branch is not merged but from the remote";
                c.result.gone_locals.insert(branch_name.to_string());
            }
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

        if let Some(upstream) = get_fetch_upstream(repo, config, base)? {
            result.insert(upstream);
        }

        if let Some(upstream) = get_push_upstream(repo, config, base)? {
            result.insert(upstream);
        }
    }
    Ok(result)
}

fn resolve_base_upstream(
    repo: &Repository,
    config: &GitConfig,
    bases: &[&str],
) -> Result<Vec<String>> {
    let mut result = Vec::new();
    for base in bases {
        // find "master -> refs/remotes/origin/master"
        if let Some(upstream) = get_fetch_upstream(repo, config, base)? {
            result.push(upstream);
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
                if let Some(upstream) = get_fetch_upstream(repo, config, branch_name)? {
                    result.insert(upstream);
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
        let ref_on_remote = get_remote_branch_from_ref(repo, remote_ref)?;
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
