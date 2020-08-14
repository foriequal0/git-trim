use std::collections::HashSet;
use std::fmt::{Debug, Display, Formatter};
use std::hash::Hash;
use std::iter::FromIterator;
use std::mem::discriminant;
use std::process::exit;
use std::str::FromStr;

use clap::Clap;

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

    /// Comma separated values of `<filter unit>[:<remote name>]`.
    /// Filter unit is one of the `all, merged, local, remote, merged-local, merged-remote, stray, diverged`.
    /// You can scope a filter unit to specific remote `:<remote name>` to a `filter unit` when the filter unit implies `merged-remote` or `diverged`.
    /// [default : `merged:origin`] [config: trim.filter]
    ///
    /// If there are filter units that are scoped, it trims remote branches only in the specified remote.
    /// If there are any filter unit that isn`t scoped, it trims all remote branches.
    ///
    /// `all` implies `merged-local,merged-remote,stray-local,stray-remote`.
    /// `merged` implies `merged-local,merged-remote`.
    /// `local` implies `merged-local,stray-local`.
    /// `remote` implies `merged-remote,stray-remote`.
    #[clap(short, long, value_delimiter = ",")]
    pub delete: Vec<Delete>,

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

#[derive(Hash, Eq, PartialEq, Clone, Debug)]
pub enum Delete {
    All(Scope),
    Merged(Scope),
    MergedLocal,
    MergedRemote(Scope),
    Stray,
    Diverged(Scope),
    Local,
    Remote(Scope),
}

#[derive(Hash, Eq, PartialEq, Clone, Debug)]
pub enum FilterUnit {
    MergedLocal,
    MergedRemote(Scope),
    Stray,
    Diverged(Scope),
}

impl FromStr for Delete {
    type Err = DeleteParseError;

    fn from_str(arg: &str) -> Result<Delete, Self::Err> {
        use Scope::*;
        let some_pair: Vec<_> = arg.splitn(2, ':').map(str::trim).collect();
        match *some_pair.as_slice() {
            ["all"] => Ok(Delete::All(All)),
            ["all", remote] => Ok(Delete::All(Scoped(remote.to_owned()))),
            ["merged"] => Ok(Delete::Merged(All)),
            ["merged", remote] => Ok(Delete::Merged(Scoped(remote.to_owned()))),
            ["stray"] => Ok(Delete::Stray),
            ["diverged"] => Ok(Delete::Diverged(All)),
            ["diverged", remote] => Ok(Delete::Diverged(Scoped(remote.to_owned()))),
            ["merged-local"] => Ok(Delete::MergedLocal),
            ["merged-remote"] => Ok(Delete::MergedRemote(All)),
            ["merged-remote", remote] => Ok(Delete::MergedRemote(Scoped(remote.to_owned()))),
            ["local"] => Ok(Delete::Local),
            ["remote"] => Ok(Delete::Remote(All)),
            ["remote", remote] => Ok(Delete::Remote(Scoped(remote.to_owned()))),
            _ => Err(DeleteParseError {
                message: format!("Unexpected delete filter: {}", arg),
            }),
        }
    }
}

impl Delete {
    fn to_filter_units(&self) -> Vec<FilterUnit> {
        match self {
            Delete::All(scope) => vec![
                FilterUnit::MergedLocal,
                FilterUnit::MergedRemote(scope.clone()),
                FilterUnit::Stray,
                FilterUnit::Diverged(scope.clone()),
            ],
            Delete::Merged(scope) => vec![
                FilterUnit::MergedLocal,
                FilterUnit::MergedRemote(scope.clone()),
            ],
            Delete::MergedLocal => vec![FilterUnit::MergedLocal],
            Delete::MergedRemote(scope) => vec![FilterUnit::MergedRemote(scope.clone())],
            Delete::Stray => vec![FilterUnit::Stray],
            Delete::Diverged(scope) => vec![FilterUnit::Diverged(scope.clone())],
            Delete::Local => vec![FilterUnit::MergedLocal, FilterUnit::Stray],
            Delete::Remote(scope) => vec![
                FilterUnit::MergedRemote(scope.clone()),
                FilterUnit::Diverged(scope.clone()),
            ],
        }
    }

    pub fn merged_origin() -> Vec<Self> {
        use Delete::*;
        vec![
            MergedLocal,
            MergedRemote(Scope::Scoped("origin".to_string())),
        ]
    }
}

#[derive(Debug)]
pub struct DeleteParseError {
    message: String,
}

unsafe impl Sync for DeleteParseError {}

impl Display for DeleteParseError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "DeleteParseError: {}", &self.message)
    }
}

impl std::error::Error for DeleteParseError {}

#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub struct DeleteFilter(HashSet<FilterUnit>);

impl DeleteFilter {
    pub fn delete_merged_local(&self) -> bool {
        self.0.contains(&FilterUnit::MergedLocal)
    }

    pub fn delete_merged_remote(&self, remote: &str) -> bool {
        for filter in self.0.iter() {
            match filter {
                FilterUnit::MergedRemote(Scope::All) => return true,
                FilterUnit::MergedRemote(Scope::Scoped(specific)) if specific == remote => {
                    return true
                }
                _ => {}
            }
        }
        false
    }

    pub fn delete_stray(&self) -> bool {
        self.0.contains(&FilterUnit::Stray)
    }

    pub fn delete_diverged(&self, remote: &str) -> bool {
        for filter in self.0.iter() {
            match filter {
                FilterUnit::Diverged(Scope::All) => return true,
                FilterUnit::Diverged(Scope::Scoped(specific)) if specific == remote => return true,
                _ => {}
            }
        }
        false
    }
}

impl FromIterator<FilterUnit> for DeleteFilter {
    fn from_iter<I>(iter: I) -> Self
    where
        I: IntoIterator<Item = FilterUnit>,
    {
        use FilterUnit::*;
        use Scope::*;

        let mut result = HashSet::new();
        for filter in iter.into_iter() {
            match filter {
                MergedLocal | Stray => {
                    result.insert(filter.clone());
                }
                MergedRemote(All) | Diverged(All) => {
                    result.retain(|x| discriminant(x) != discriminant(&filter));
                    result.insert(filter.clone());
                }
                MergedRemote(_) => {
                    if !result.contains(&MergedRemote(All)) {
                        result.insert(filter.clone());
                    }
                }
                Diverged(_) => {
                    if !result.contains(&Diverged(All)) {
                        result.insert(filter.clone());
                    }
                }
            }
        }

        Self(result)
    }
}

impl FromIterator<Delete> for DeleteFilter {
    fn from_iter<I>(iter: I) -> Self
    where
        I: IntoIterator<Item = Delete>,
    {
        Self::from_iter(iter.into_iter().map(|x| x.to_filter_units()).flatten())
    }
}
