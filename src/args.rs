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
    /// [default: master] [config: trim.bases]
    #[clap(short, long, value_delimiter = ",", aliases=&["base"])]
    pub bases: Vec<String>,

    /// Comma separated multiple glob patterns (e.g. `release-*`, `feature/*`) of branches that should never be deleted.
    /// [default: <bases>] [config: trim.protected]
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
    /// Filter unit is one of the `all, merged, gone, local, remote, merged-local, merged-remote, stray-local, stray-remote`.
    /// You can scope a filter unit to specific remote `:<remote name>` to a `filter unit` when the filter unit implies `merged-remote` or `stray-remote`.
    /// [default : `merged:origin`] [config: trim.filter]
    ///
    /// If there are filter units that are scoped, it trims remote branches only in the specified remote.
    /// If there are any filter unit that isn`t scoped, it trims all remote branches.
    ///
    /// `all` implies `merged-local,merged-remote,stray-local,stray-remote`.
    /// `merged` implies `merged-local,merged-remote`.
    /// `stray` implies `stray-local,stray-remote`.
    /// `local` implies `merged-local,stray-local`.
    /// `remote` implies `merged-remote,stray-remote`.
    #[clap(short, long)]
    pub delete: Vec<DeleteFilter>,

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
pub enum FilterUnit {
    MergedLocal,
    MergedRemote(Scope),
    StrayLocal,
    StrayRemote(Scope),
}

#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub struct DeleteFilter(HashSet<FilterUnit>);

impl DeleteFilter {
    pub fn merged_origin() -> Self {
        use FilterUnit::*;
        DeleteFilter(HashSet::from_iter(vec![
            MergedLocal,
            MergedRemote(Scope::Scoped("origin".to_string())),
        ]))
    }

    pub fn all() -> Self {
        use FilterUnit::*;
        DeleteFilter(HashSet::from_iter(vec![
            MergedLocal,
            MergedRemote(Scope::All),
            StrayLocal,
            StrayRemote(Scope::All),
        ]))
    }

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

    pub fn delete_stray_local(&self) -> bool {
        self.0.contains(&FilterUnit::StrayLocal)
    }

    pub fn delete_stray_remote(&self, remote: &str) -> bool {
        for filter in self.0.iter() {
            match filter {
                FilterUnit::StrayRemote(Scope::All) => return true,
                FilterUnit::StrayRemote(Scope::Scoped(specific)) if specific == remote => {
                    return true
                }
                _ => {}
            }
        }
        false
    }
}

impl FromStr for DeleteFilter {
    type Err = DeleteFilterParseError;

    fn from_str(args: &str) -> Result<DeleteFilter, Self::Err> {
        use FilterUnit::*;
        use Scope::*;
        let mut result: Vec<FilterUnit> = Vec::new();
        for arg in args.split(',') {
            let some_pair: Vec<_> = arg.splitn(2, ':').map(str::trim).collect();
            let filters = match *some_pair.as_slice() {
                ["all"] => vec![MergedLocal, MergedRemote(All), StrayLocal, StrayRemote(All)],
                ["all", remote] => vec![
                    MergedLocal,
                    MergedRemote(Scoped(remote.to_string())),
                    StrayLocal,
                    StrayRemote(Scoped(remote.to_string())),
                ],
                ["merged"] => vec![MergedLocal, MergedRemote(All)],
                ["merged", remote] => vec![MergedLocal, MergedRemote(Scoped(remote.to_string()))],
                ["stray"] => vec![StrayLocal, StrayRemote(All)],
                ["stray", remote] => vec![StrayLocal, StrayRemote(Scoped(remote.to_string()))],
                ["local"] => vec![MergedLocal, StrayLocal],
                ["remote"] => vec![MergedRemote(All), StrayRemote(All)],
                ["remote", remote] => vec![
                    MergedRemote(Scoped(remote.to_string())),
                    StrayRemote(Scoped(remote.to_string())),
                ],
                ["merged-local"] => vec![MergedLocal],
                ["merged-remote"] => vec![MergedRemote(All)],
                ["merged-remote", remote] => vec![MergedRemote(Scoped(remote.to_string()))],
                ["stray-local"] => vec![StrayLocal],
                ["stray-remote", remote] => vec![StrayRemote(Scoped(remote.to_string()))],
                _ if arg.is_empty() => vec![],
                _ => {
                    return Err(DeleteFilterParseError {
                        message: format!("Unexpected delete filter: {}", arg),
                    });
                }
            };
            result.extend(filters);
        }

        Ok(Self(HashSet::from_iter(result)))
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
                MergedLocal | StrayLocal => {
                    result.insert(filter.clone());
                }
                MergedRemote(All) | StrayRemote(All) => {
                    result.retain(|x| discriminant(x) != discriminant(&filter));
                    result.insert(filter.clone());
                }
                MergedRemote(_) => {
                    if !result.contains(&MergedRemote(All)) {
                        result.insert(filter.clone());
                    }
                }
                StrayRemote(_) => {
                    if !result.contains(&StrayRemote(All)) {
                        result.insert(filter.clone());
                    }
                }
            }
        }

        Self(result)
    }
}

impl FromIterator<Self> for DeleteFilter {
    fn from_iter<I>(iter: I) -> Self
    where
        I: IntoIterator<Item = Self>,
    {
        Self(iter.into_iter().flatten().collect())
    }
}

impl IntoIterator for DeleteFilter {
    type Item = FilterUnit;
    type IntoIter = std::collections::hash_set::IntoIter<FilterUnit>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

#[derive(Debug)]
pub struct DeleteFilterParseError {
    message: String,
}

unsafe impl Sync for DeleteFilterParseError {}

impl Display for DeleteFilterParseError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "DeleteFilterParseError: {}", &self.message)
    }
}

impl std::error::Error for DeleteFilterParseError {}
