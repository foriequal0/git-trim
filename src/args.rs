use std::collections::HashSet;
use std::fmt::Debug;
use std::hash::Hash;
use std::iter::FromIterator;
use std::mem::discriminant;
use std::process::exit;
use std::str::FromStr;

use clap::Clap;
use thiserror::Error;

#[derive(Clap, Default)]
#[clap(
    version,
    about = "Automatically trims your tracking branches whose upstream branches are merged or stray.",
    long_about = "Automatically trims your tracking branches whose upstream branches are merged or stray.
`git-trim` is a missing companion to the `git fetch --prune` and a proper, safer, faster alternative to your `<bash oneliner HERE>`."
)]
pub struct Args {
    /// Comma separated multiple names of branches.
    /// All the other branches are compared with the upstream branches of those branches.
    /// [default: branches that tracks `git symbolic-ref refs/remotes/*/HEAD`] [config: trim.bases]
    ///
    /// The default value is a branch that tracks `git symbolic-ref refs/remotes/*/HEAD`.
    /// They might not be reflected correctly when the HEAD branch of your remote repository is changed.
    /// You can see the changed HEAD branch name with `git remote show <remote>`
    /// and apply it to your local repository with `git remote set-head <remote> --auto`.
    #[clap(short, long, value_delimiter = ",", aliases=&["base"])]
    pub bases: Vec<String>,

    /// Comma separated multiple glob patterns (e.g. `release-*`, `feature/*`) of branches that should never be deleted.
    /// [config: trim.protected]
    #[clap(short, long, value_delimiter = ",")]
    pub protected: Vec<String>,

    /// Do not update remotes
    /// [config: trim.update]
    #[clap(long)]
    pub no_update: bool,
    #[clap(long, hidden(true))]
    pub update: bool,

    /// Prevents too frequent updates. Seconds between updates in seconds. 0 to disable.
    /// [default: 5] [config: trim.updateInterval]
    #[clap(long)]
    pub update_interval: Option<u64>,

    /// Do not ask confirm
    /// [config: trim.confirm]
    #[clap(long)]
    pub no_confirm: bool,
    #[clap(long, hidden(true))]
    pub confirm: bool,

    /// Do not detach when HEAD is about to be deleted
    /// [config: trim.detach]
    #[clap(long)]
    pub no_detach: bool,
    #[clap(long, hidden(true))]
    pub detach: bool,

    /// Comma separated values of `<delete range>[:<remote name>]`.
    /// Delete range is one of the `merged, merged-local, merged-remote, stray, diverged, local, remote`.
    /// `:<remote name>` is necessary to a `<delete range>` when the delete range implies `merged-remote`, `diverged` or `remote`.
    /// You can use `*` as `<remote name>` to delete a range of branches from all remotes.
    /// [default : `merged:origin`] [config: trim.delete]
    ///
    /// `merged` implies `merged-local,merged-remote`.
    ///
    /// When `local` is specified, deletes non-tracking merged local branches.
    /// When `remote` is specified, deletes non-upstream merged remote tracking branches.
    #[clap(short, long, value_delimiter = ",")]
    pub delete: Vec<DeleteRange>,

    /// Do not delete branches, show what branches will be deleted.
    #[clap(long)]
    pub dry_run: bool,
}

impl Args {
    pub fn update(&self) -> Option<bool> {
        exclusive_bool(("update", self.update), ("no-update", self.no_update))
    }

    pub fn confirm(&self) -> Option<bool> {
        exclusive_bool(("confirm", self.confirm), ("no-confirm", self.no_confirm))
    }

    pub fn detach(&self) -> Option<bool> {
        exclusive_bool(("detach", self.detach), ("no-detach", self.no_detach))
    }
}

impl paw::ParseArgs for Args {
    /// Error type.
    type Error = std::io::Error;

    /// Try to parse an input to a type.
    fn parse_args() -> Result<Self, Self::Error> {
        Ok(Args::parse())
    }
}

fn exclusive_bool(
    (name_pos, value_pos): (&str, bool),
    (name_neg, value_neg): (&str, bool),
) -> Option<bool> {
    if value_pos && value_neg {
        eprintln!(
            "Error: Flag '{}' and '{}' cannot be used simultaneously",
            name_pos, name_neg,
        );
        exit(-1);
    }

    if value_pos {
        Some(true)
    } else if value_neg {
        Some(false)
    } else {
        None
    }
}

#[derive(Hash, Eq, PartialEq, Clone, Debug)]
pub enum Scope {
    All,
    Scoped(String),
}

impl FromStr for Scope {
    type Err = ScopeParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "" => Err(ScopeParseError {
                message: "Scope is empty".to_owned(),
            }),
            "*" => Ok(Scope::All),
            scope => Ok(Scope::Scoped(scope.to_owned())),
        }
    }
}

#[derive(Error, Debug)]
#[error("{message}")]
pub struct ScopeParseError {
    message: String,
}

#[derive(Hash, Eq, PartialEq, Clone, Debug)]
pub enum DeleteRange {
    Merged(Scope),
    MergedLocal,
    MergedRemote(Scope),
    Stray,
    Diverged(Scope),
    Local,
    Remote(Scope),
}

#[derive(Hash, Eq, PartialEq, Clone, Debug)]
pub enum DeleteUnit {
    MergedLocal,
    MergedRemote(Scope),
    Stray,
    Diverged(Scope),
    MergedNonTrackingLocal,
    MergedNonUpstreamRemoteTracking(Scope),
}

impl FromStr for DeleteRange {
    type Err = DeleteParseError;

    fn from_str(arg: &str) -> Result<DeleteRange, Self::Err> {
        let some_pair: Vec<_> = arg.splitn(2, ':').map(str::trim).collect();
        match *some_pair.as_slice() {
            ["merged", remote] => Ok(DeleteRange::Merged(remote.parse()?)),
            ["stray"] => Ok(DeleteRange::Stray),
            ["diverged", remote] => Ok(DeleteRange::Diverged(remote.parse()?)),
            ["merged-local"] => Ok(DeleteRange::MergedLocal),
            ["merged-remote", remote] => Ok(DeleteRange::MergedRemote(remote.parse()?)),
            ["local"] => Ok(DeleteRange::Local),
            ["remote", remote] => Ok(DeleteRange::Remote(remote.parse()?)),
            _ => Err(DeleteParseError::InvalidDeleteRangeFormat(arg.to_owned())),
        }
    }
}

impl DeleteRange {
    fn to_delete_units(&self) -> Vec<DeleteUnit> {
        match self {
            DeleteRange::Merged(scope) => vec![
                DeleteUnit::MergedLocal,
                DeleteUnit::MergedRemote(scope.clone()),
            ],
            DeleteRange::MergedLocal => vec![DeleteUnit::MergedLocal],
            DeleteRange::MergedRemote(scope) => vec![DeleteUnit::MergedRemote(scope.clone())],
            DeleteRange::Stray => vec![DeleteUnit::Stray],
            DeleteRange::Diverged(scope) => vec![DeleteUnit::Diverged(scope.clone())],
            DeleteRange::Local => vec![DeleteUnit::MergedNonTrackingLocal],
            DeleteRange::Remote(scope) => {
                vec![DeleteUnit::MergedNonUpstreamRemoteTracking(scope.clone())]
            }
        }
    }

    pub fn merged_origin() -> Vec<Self> {
        use DeleteRange::*;
        vec![
            MergedLocal,
            MergedRemote(Scope::Scoped("origin".to_string())),
        ]
    }
}

#[derive(Error, Debug)]
pub enum DeleteParseError {
    #[error("Invalid delete range format `{0}`")]
    InvalidDeleteRangeFormat(String),
    #[error("Scope parse error for delete range while parsing scope: {0}")]
    ScopeParseError(#[from] ScopeParseError),
}

#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub struct DeleteFilter(HashSet<DeleteUnit>);

impl DeleteFilter {
    pub fn scan_tracking(&self) -> bool {
        self.0.iter().any(|unit| {
            matches!(unit,
                DeleteUnit::MergedLocal
                | DeleteUnit::MergedRemote(_)
                | DeleteUnit::Stray
                | DeleteUnit::Diverged(_))
        })
    }

    pub fn scan_non_tracking_local(&self) -> bool {
        self.0.contains(&DeleteUnit::MergedNonTrackingLocal)
    }

    pub fn scan_non_upstream_remote(&self, remote: &str) -> bool {
        for unit in self.0.iter() {
            match unit {
                DeleteUnit::MergedNonUpstreamRemoteTracking(Scope::All) => return true,
                DeleteUnit::MergedNonUpstreamRemoteTracking(Scope::Scoped(specific))
                    if specific == remote =>
                {
                    return true
                }
                _ => {}
            }
        }
        false
    }

    pub fn delete_merged_local(&self) -> bool {
        self.0.contains(&DeleteUnit::MergedLocal)
    }

    pub fn delete_merged_remote(&self, remote: &str) -> bool {
        for unit in self.0.iter() {
            match unit {
                DeleteUnit::MergedRemote(Scope::All) => return true,
                DeleteUnit::MergedRemote(Scope::Scoped(specific)) if specific == remote => {
                    return true
                }
                _ => {}
            }
        }
        false
    }

    pub fn delete_stray(&self) -> bool {
        self.0.contains(&DeleteUnit::Stray)
    }

    pub fn delete_diverged(&self, remote: &str) -> bool {
        for unit in self.0.iter() {
            match unit {
                DeleteUnit::Diverged(Scope::All) => return true,
                DeleteUnit::Diverged(Scope::Scoped(specific)) if specific == remote => return true,
                _ => {}
            }
        }
        false
    }

    pub fn delete_merged_non_tracking_local(&self) -> bool {
        self.0.contains(&DeleteUnit::MergedNonTrackingLocal)
    }

    pub fn delete_merged_non_upstream_remote_tracking(&self, remote: &str) -> bool {
        for filter in self.0.iter() {
            match filter {
                DeleteUnit::MergedNonUpstreamRemoteTracking(Scope::All) => return true,
                DeleteUnit::MergedNonUpstreamRemoteTracking(Scope::Scoped(specific))
                    if specific == remote =>
                {
                    return true
                }
                _ => {}
            }
        }
        false
    }
}

impl FromIterator<DeleteUnit> for DeleteFilter {
    fn from_iter<I>(iter: I) -> Self
    where
        I: IntoIterator<Item = DeleteUnit>,
    {
        use DeleteUnit::*;
        use Scope::*;

        let mut result = HashSet::new();
        for unit in iter.into_iter() {
            match unit {
                MergedLocal | Stray | MergedNonTrackingLocal => {
                    result.insert(unit.clone());
                }
                MergedRemote(All) | Diverged(All) | MergedNonUpstreamRemoteTracking(All) => {
                    result.retain(|x| discriminant(x) != discriminant(&unit));
                    result.insert(unit.clone());
                }
                MergedRemote(_) => {
                    if !result.contains(&MergedRemote(All)) {
                        result.insert(unit.clone());
                    }
                }
                Diverged(_) => {
                    if !result.contains(&Diverged(All)) {
                        result.insert(unit.clone());
                    }
                }
                MergedNonUpstreamRemoteTracking(_) => {
                    if !result.contains(&MergedNonUpstreamRemoteTracking(All)) {
                        result.insert(unit.clone());
                    }
                }
            }
        }

        Self(result)
    }
}

impl FromIterator<DeleteRange> for DeleteFilter {
    fn from_iter<I>(iter: I) -> Self
    where
        I: IntoIterator<Item = DeleteRange>,
    {
        Self::from_iter(iter.into_iter().map(|x| x.to_delete_units()).flatten())
    }
}
