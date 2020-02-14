pub mod args;

use std::collections::{HashMap, HashSet};
use std::ops::Deref;
use std::process::{Command, Stdio};

use git2::{BranchType, Direction, ErrorClass, ErrorCode, Repository};
use log::*;

use crate::args::{Category, DeleteFilter};

type Result<T> = ::std::result::Result<T, Error>;
type Error = Box<dyn std::error::Error>;

pub fn git(args: &[&str]) -> Result<()> {
    info!("> git {}", args.join(" "));
    let exit_status = Command::new("git").args(args).status()?;
    if !exit_status.success() {
        Err(std::io::Error::from_raw_os_error(exit_status.code().unwrap_or(-1)).into())
    } else {
        Ok(())
    }
}

fn git_output(args: &[&str]) -> Result<String> {
    info!("> git {}", args.join(" "));
    let output = Command::new("git")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::from_raw_os_error(output.status.code().unwrap_or(-1)).into());
    }

    let str = std::str::from_utf8(&output.stdout)?.trim();
    Ok(str.to_string())
}

fn is_merged(base: &str, branch: &str) -> Result<bool> {
    let range = format!("{}...{}", base, branch);
    // Is there any revs that are not applied to the base in the branch?
    let output = git_output(&[
        "rev-list",
        "--cherry-pick",
        "--right-only",
        "--no-merges",
        "-n1",
        &range,
    ])?;

    // empty output means there aren't any revs that are not applied to the base.
    if output.is_empty() {
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Source: https://stackoverflow.com/a/56026209
fn is_squash_merged(base: &str, branch: &str) -> Result<bool> {
    let merge_base = git_output(&["merge-base", base, branch])?;
    let tree = git_output(&["rev-parse", &format!("{}^{{tree}}", branch)])?;
    let dangling_commit = git_output(&[
        "commit-tree",
        &tree,
        "-p",
        &merge_base,
        "-m",
        "git-trim: squash merge test",
    ])?;
    is_merged(base, &dangling_commit)
}

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
        let head_name = head.name().ok_or("non-utf8 head ref name")?;
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

pub fn get_config_update(repo: &Repository, given: Option<bool>) -> Result<bool> {
    if let Some(given) = given {
        return Ok(given);
    }
    let config = repo.config()?;
    match config.get_bool("trim.update") {
        Ok(value) => Ok(value),
        Err(err) if config_not_exist(&err) => Ok(true),
        Err(err) => Err(err.into()),
    }
}

#[derive(Debug)]
pub enum ConfigValue<T> {
    Explicit { value: T, source: String },
    Implicit(T),
}

impl<T> Deref for ConfigValue<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        match self {
            ConfigValue::Explicit { value: x, .. } | ConfigValue::Implicit(x) => x,
        }
    }
}

impl<T> ConfigValue<T> {
    pub fn map<F, U>(&self, func: F) -> ConfigValue<U>
    where
        F: Fn(&T) -> U,
    {
        match self {
            ConfigValue::Explicit { value, source } => ConfigValue::Explicit {
                value: func(value),
                source: source.clone(),
            },
            ConfigValue::Implicit(x) => ConfigValue::Implicit(func(x)),
        }
    }
}

pub fn get_config_string(
    repo: &Repository,
    given: Option<&String>,
    key: &str,
    default: &str,
) -> Result<ConfigValue<String>> {
    if let Some(given) = given {
        return Ok(ConfigValue::Explicit {
            value: given.clone(),
            source: "cli".to_string(),
        });
    }
    let config = repo.config()?;
    match config.get_string(key) {
        Ok(value) => Ok(ConfigValue::Explicit {
            value,
            source: key.to_string(),
        }),
        Err(err) if config_not_exist(&err) => Ok(ConfigValue::Implicit(default.to_string())),
        Err(err) => Err(err.into()),
    }
}

pub fn get_config_bool(
    repo: &Repository,
    given: Option<bool>,
    key: &str,
    default: bool,
) -> Result<ConfigValue<bool>> {
    if let Some(given) = given {
        return Ok(ConfigValue::Explicit {
            value: given,
            source: "cli".to_string(),
        });
    }
    let config = repo.config()?;
    match config.get_bool(key) {
        Ok(value) => Ok(ConfigValue::Explicit {
            value,
            source: key.to_string(),
        }),
        Err(err) if config_not_exist(&err) => Ok(ConfigValue::Implicit(default)),
        Err(err) => Err(err.into()),
    }
}

// given refspec for a remote: refs/heads/*:refs/remotes/origin
// master -> refs/remotes/origin/master
// refs/head/master -> refs/remotes/origin/master
fn get_fetch_remote_ref(repo: &Repository, branch: &str) -> Result<Option<String>> {
    let remote_name = get_remote(repo, branch)?;
    get_remote_ref(repo, &remote_name, branch)
}

fn get_remote_ref(repo: &Repository, remote_name: &str, branch: &str) -> Result<Option<String>> {
    let remote = repo.find_remote(remote_name)?;
    for refspec in remote.refspecs() {
        if let Direction::Push = refspec.direction() {
            continue;
        }
        let src = refspec.src().ok_or("non-utf8 src dst")?;
        let dst = refspec.dst().ok_or("non-utf8 refspec dst")?;
        assert!(src.ends_with('*'), "Unsupported src refspec");
        let name = if branch.starts_with("refs/") && branch.starts_with(&src[..src.len() - 1]) {
            &branch[src.len() - 1..]
        } else {
            branch
        };
        let expanded = dst.replace("*", name);

        let exists = repo.find_reference(&expanded).is_ok();
        if exists {
            return Ok(Some(expanded));
        }
    }
    Ok(None)
}

#[derive(Eq, PartialEq)]
struct RefOnRemote {
    remote_name: String,
    refname: String,
}

// given refspec for a remote: refs/heads/*:refs/heads/*
// master -> refs/remotes/origin/master
// refs/head/master -> refs/remotes/origin/master
fn get_push_ref_on_remote(repo: &Repository, branch: &str) -> Result<Option<RefOnRemote>> {
    fn get_ref_on_remote(
        repo: &Repository,
        remote_name: &str,
        branch: &str,
        src: &str,
        dst: &str,
    ) -> Result<Option<RefOnRemote>> {
        let reference = repo.resolve_reference_from_short_name(branch)?;
        let refname = reference.name().ok_or("non-utf8 ref")?;
        let relative_ref = if refname.starts_with(&src[..src.len() - 1]) {
            &refname[src.len() - 1..]
        } else {
            return Ok(None);
        };
        let expanded = dst.replace("*", relative_ref);

        Ok(Some(RefOnRemote {
            remote_name: remote_name.to_string(),
            refname: expanded,
        }))
    }
    let config = repo.config()?;
    let push_default = match config.get_string("push.default") {
        Ok(value) => value,
        Err(err) if config_not_exist(&err) => "simple".to_string(),
        Err(err) => return Err(err.into()),
    };

    match push_default.as_str() {
        "current" => {
            let remote_name = get_push_remote(repo, branch)?;
            get_ref_on_remote(repo, &remote_name, branch, "refs/heads/*", "refs/heads/*")
        }
        "upstream" | "tracking" | "simple" => {
            let branch = repo.find_branch(branch, BranchType::Local)?;
            let branch_name = branch.name()?.ok_or("non-utf8 branch name")?;
            let upstream = branch.upstream()?;
            if push_default.as_str() == "simple" && Some(branch_name) != upstream.name()? {
                panic!("The current branch foo has no upstream branch")
            }
            let remote_name = get_push_remote(repo, branch_name)?;
            let remote = repo.find_remote(&remote_name)?;
            for refspec in remote.refspecs() {
                if let Direction::Fetch = refspec.direction() {
                    continue;
                }
                let src = refspec.src().ok_or("non-utf8 src dst")?;
                let dst = refspec.dst().ok_or("non-utf8 refspec dst")?;
                assert!(src.ends_with('*'), "Unsupported src refspec");
                if let Some(result) = get_ref_on_remote(repo, &remote_name, branch_name, src, dst)?
                {
                    return Ok(Some(result));
                }
            }
            panic!("refspec doesn't exist");
        }
        "nothing" | "matching" => unimplemented!("push.default=matching is not implemented"),
        _ => panic!("unexpected config push.default"),
    }
}

fn get_push_remote_ref(repo: &Repository, branch: &str) -> Result<Option<String>> {
    if let Some(RefOnRemote {
        remote_name,
        refname,
    }) = get_push_ref_on_remote(repo, branch)?
    {
        if let Some(remote_ref) = get_remote_ref(repo, &remote_name, &refname)? {
            return Ok(Some(remote_ref));
        }
    }
    Ok(None)
}

fn get_push_remote(repo: &Repository, branch: &str) -> Result<ConfigValue<String>> {
    let config = repo.config()?;

    let source = format!("branch.{}.pushRemote", branch);
    match config.get_string(&source) {
        Ok(value) => return Ok(ConfigValue::Explicit { value, source }),
        Err(err) if !config_not_exist(&err) => return Err(err.into()),
        _ => {}
    }

    let source = "remote.pushDefault";
    match config.get_string(source) {
        Ok(value) => {
            return Ok(ConfigValue::Explicit {
                value,
                source: source.to_string(),
            })
        }
        Err(err) if !config_not_exist(&err) => return Err(err.into()),
        _ => {}
    }

    get_remote(repo, branch)
}

fn get_remote(repo: &Repository, branch: &str) -> Result<ConfigValue<String>> {
    let config = repo.config()?;

    let source = format!("branch.{}.remote", branch);
    match config.get_string(&source) {
        Ok(value) => return Ok(ConfigValue::Explicit { value, source }),
        Err(err) if !config_not_exist(&err) => return Err(err.into()),
        _ => {}
    }

    Ok(ConfigValue::Implicit("origin".to_string()))
}

fn config_not_exist(err: &git2::Error) -> bool {
    err.code() == ErrorCode::NotFound && err.class() == ErrorClass::Config
}

pub fn get_merged_or_gone(repo: &Repository, base: &str) -> Result<MergedOrGone> {
    let base_remote_ref = resolve_config_base_ref(repo, base)?;
    let mut result = MergedOrGone::default();
    for branch in repo.branches(Some(BranchType::Local))? {
        let (branch, _) = branch?;
        let branch_name = branch.name()?.ok_or("non-utf8 branch name")?;
        debug!("Branch: {:?}", branch.name()?);
        if let ConfigValue::Implicit(_) = get_remote(repo, branch_name)? {
            debug!(
                "Skip: the branch doesn't have a tracking remote: {:?}",
                branch_name
            );
            continue;
        }
        if let Ok(upstream) = branch.upstream() {
            let remote_ref = upstream.get().name();
            if remote_ref == Some(&base_remote_ref) {
                debug!("Skip: the branch is the base: {:?}", branch_name);
                continue;
            }
        }
        let reference = branch.get();
        if reference.symbolic_target().is_some() {
            debug!("Skip: the branch is a symbolic ref: {:?}", branch_name);
            continue;
        }
        let merged = is_merged(&base_remote_ref, branch_name)?
            || is_squash_merged(&base_remote_ref, branch_name)?;
        let fetch = get_fetch_remote_ref(repo, branch_name)?;
        let push = get_push_remote_ref(repo, branch_name)?;
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

            (Some(_), None) => {
                debug!("skip: it might be a long running branch like 'develop' but never pushed to the personal repo in the triangular workflow");
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

fn resolve_config_base_ref(repo: &Repository, base: &str) -> Result<String> {
    // find "master -> refs/remotes/origin/master"
    if let Some(remote_ref) = get_fetch_remote_ref(&repo, base)? {
        trace!("Found fetch remote ref for: {}, {}", base, remote_ref);
        return Ok(remote_ref);
    }

    // match "origin/master -> refs/remotes/origin/master"
    if let Ok(remote_ref) = repo.find_reference(&format!("refs/remotes/{}", base)) {
        let refname = remote_ref.name().ok_or("non-utf8 reference name")?;
        trace!("Found remote ref for: {}, {}", base, refname);
        return Ok(refname.to_string());
    }

    trace!("Not found remote refs. fallback: {}", base);
    Ok(repo
        .find_reference(base)?
        .name()
        .ok_or("non-utf8 ref")?
        .to_string())
}

pub fn delete_local_branches(repo: &Repository, branches: &[&str], dry_run: bool) -> Result<()> {
    if branches.is_empty() {
        return Ok(());
    }
    let mut args = vec!["branch", "--delete", "--force"];
    args.extend(branches);

    let detach_to = if repo.head_detached()? {
        None
    } else {
        let head = repo.head()?;
        let head_refname = head.name().ok_or("non-utf8 head ref name")?;
        assert!(head_refname.starts_with("refs/heads/"));
        let head_name = &head_refname["refs/heads/".len()..];
        if branches.contains(&head_name) {
            Some(head)
        } else {
            None
        }
    };

    if dry_run {
        if let Some(head) = detach_to {
            let head_refname = head.name().ok_or("non-utf8 head ref name")?;
            info!("> git checkout {} (dry-run)", head_refname);

            println!("Note: switching to '{}' (dry run)", head_refname);
            println!("You are in 'detached HED' state... blah blah...");
            let commit = head.peel_to_commit()?;
            let message = commit.message().ok_or("non-utf8 head ref name")?;
            println!(
                "HEAD is now at {} {} (dry run)",
                &commit.id().to_string()[..7],
                message.lines().next().unwrap_or_default()
            );
        }
        for branch in branches {
            info!("> git {} (dry-run)", args.join(" "));
            println!("Delete branch {} (dry run).", branch);
        }
    } else {
        if let Some(head) = detach_to {
            let head_refname = head.name().ok_or("non-utf8 head ref name")?;
            git(&["checkout", head_refname])?;
        }
        git(&args)?;
    }
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
    let mut command = vec!["push", "--delete"];
    if dry_run {
        command.push("--dry-run");
    }
    for (remote_name, remote_refnames) in per_remote.iter() {
        let mut args = command.clone();
        args.push(remote_name);
        args.extend(remote_refnames.iter().map(String::as_str));
        git(&args)?;
    }
    Ok(())
}

fn get_remote_name_and_ref_on_remote(
    repo: &Repository,
    remote_ref: &str,
) -> Result<(String, String)> {
    assert!(remote_ref.starts_with("refs/remotes/"));
    for remote_name in repo.remotes()?.iter() {
        let remote_name = remote_name.ok_or("non-utf8 remote name")?;
        let remote = repo.find_remote(&remote_name)?;
        for refspec in remote.refspecs() {
            if let Direction::Push = refspec.direction() {
                continue;
            }
            let src = refspec.src().ok_or("non-utf8 src dst")?;
            let dst = refspec.dst().ok_or("non-utf8 refspec dst")?;
            assert!(dst.ends_with('*'), "Unsupported src refspec");
            if remote_ref.starts_with(&dst[..dst.len() - 1]) {
                let expanded = src.replace("*", &remote_ref[dst.len() - 1..]);
                return Ok((
                    remote.name().ok_or("non-utf8 remote name")?.to_string(),
                    expanded,
                ));
            }
        }
    }
    unreachable!("matching refspec is not found");
}
