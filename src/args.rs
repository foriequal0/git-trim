use std::collections::HashSet;
use std::fmt::{Debug, Display, Formatter};
use std::hash::Hash;
use std::iter::FromIterator;
use std::mem::discriminant;
use std::process::exit;
use std::str::FromStr;

#[derive(Hash, Eq, PartialEq, Clone, Debug)]
pub enum Scope {
    All,
    Scoped(String),
}

#[derive(Hash, Eq, PartialEq, Clone, Debug)]
pub enum FilterUnit {
    MergedLocal,
    MergedRemote(Scope),
    GoneLocal,
    GoneRemote(Scope),
}

#[derive(Debug, Clone)]
pub struct DeleteFilter(HashSet<FilterUnit>);

impl DeleteFilter {
    pub fn merged() -> Self {
        use FilterUnit::*;
        DeleteFilter::from_iter(vec![MergedLocal, MergedRemote(Scope::All)])
    }

    pub fn all() -> Self {
        use FilterUnit::*;
        DeleteFilter::from_iter(vec![
            MergedLocal,
            MergedRemote(Scope::All),
            GoneLocal,
            GoneRemote(Scope::All),
        ])
    }

    pub fn filter_merged_local(&self) -> bool {
        self.0.contains(&FilterUnit::MergedLocal)
    }

    pub fn filter_merged_remote(&self, remote: &str) -> bool {
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

    pub fn filter_gone_local(&self) -> bool {
        self.0.contains(&FilterUnit::GoneLocal)
    }

    pub fn filter_gone_remote(&self, remote: &str) -> bool {
        for filter in self.0.iter() {
            match filter {
                FilterUnit::GoneRemote(Scope::All) => return true,
                FilterUnit::GoneRemote(Scope::Scoped(specific)) if specific == remote => {
                    return true
                }
                _ => {}
            }
        }
        false
    }

    pub fn into_option(self) -> Option<Self> {
        if self.0.is_empty() {
            None
        } else {
            Some(self)
        }
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
                ["all"] => vec![MergedLocal, MergedRemote(All), GoneLocal, GoneRemote(All)],
                ["all", remote] => vec![
                    MergedLocal,
                    MergedRemote(Scoped(remote.to_string())),
                    GoneLocal,
                    GoneRemote(Scoped(remote.to_string())),
                ],
                ["merged"] => vec![MergedLocal, MergedRemote(All)],
                ["merged", remote] => vec![MergedLocal, MergedRemote(Scoped(remote.to_string()))],
                ["gone"] => vec![GoneLocal, GoneRemote(All)],
                ["gone", remote] => vec![GoneLocal, GoneRemote(Scoped(remote.to_string()))],
                ["local"] => vec![MergedLocal, GoneLocal],
                ["remote"] => vec![MergedRemote(All), GoneRemote(All)],
                ["remote", remote] => vec![
                    MergedRemote(Scoped(remote.to_string())),
                    GoneRemote(Scoped(remote.to_string())),
                ],
                ["merged-local"] => vec![MergedLocal],
                ["merged-remote"] => vec![MergedRemote(All)],
                ["merged-remote", remote] => vec![MergedRemote(Scoped(remote.to_string()))],
                ["gone-local"] => vec![GoneLocal],
                ["gone-remote", remote] => vec![GoneRemote(Scoped(remote.to_string()))],
                _ if arg.is_empty() => vec![],
                _ => {
                    return Err(DeleteFilterParseError {
                        message: format!("Unexpected delete filter: {}", arg),
                    });
                }
            };
            result.extend(filters);
        }

        Ok(Self::from_iter(result))
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
                MergedLocal | GoneLocal => {
                    result.insert(filter.clone());
                }
                MergedRemote(All) | GoneRemote(All) => {
                    result.retain(|x| discriminant(x) != discriminant(&filter));
                    result.insert(filter.clone());
                }
                MergedRemote(_) => {
                    if !result.contains(&MergedRemote(All)) {
                        result.insert(filter.clone());
                    }
                }
                GoneRemote(_) => {
                    if !result.contains(&GoneRemote(All)) {
                        result.insert(filter.clone());
                    }
                }
            }
        }

        Self(result)
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

#[derive(derive_deref::Deref, Debug, Clone, Default)]
pub struct CommaSeparatedSet<T>(Vec<T>);

impl<T> FromStr for CommaSeparatedSet<T>
where
    T: FromStr + PartialEq,
{
    type Err = T::Err;

    fn from_str(args: &str) -> Result<Self, Self::Err> {
        let mut result = Vec::new();
        for arg in args.split(',') {
            let parsed = arg.trim().parse()?;
            result.push(parsed);
        }
        Ok(Self::from_iter(result))
    }
}

impl<T> FromIterator<T> for CommaSeparatedSet<T>
where
    T: PartialEq,
{
    fn from_iter<I>(iter: I) -> Self
    where
        I: IntoIterator<Item = T>,
    {
        let mut result = Vec::new();
        for item in iter.into_iter() {
            if !result.contains(&item) {
                result.push(item);
            }
        }
        Self(result)
    }
}

impl<T> IntoIterator for CommaSeparatedSet<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<T> CommaSeparatedSet<T> {
    pub fn into_option(self) -> Option<Self> {
        if self.0.is_empty() {
            None
        } else {
            Some(self)
        }
    }
}

#[derive(structopt::StructOpt)]
pub struct Args {
    /// Comma separated or a multiple arguments of refs that other refs are compared to determine whether it is merged or gone. [default: master] [config: trim.base]
    #[structopt(short, long, aliases=&["base"])]
    pub bases: Vec<CommaSeparatedSet<String>>,

    // Comma separated or a multiple arguments of glob pattern of branches that never be deleted.
    #[structopt(short, long)]
    pub protected: Vec<CommaSeparatedSet<String>>,

    /// Not update remotes [config: trim.update]
    #[structopt(long)]
    pub no_update: bool,
    #[structopt(long, hidden(true))]
    pub update: bool,

    /// Do not ask confirm [config: trim.confirm]
    #[structopt(long)]
    pub no_confirm: bool,
    #[structopt(long, hidden(true))]
    pub confirm: bool,

    /// Do not detach when HEAD is about to be deleted [config: trim.detach]
    #[structopt(long)]
    pub no_detach: bool,
    #[structopt(long, hidden(true))]
    pub detach: bool,

    /// Comma separated values of '<filter unit>[:<remote name>]'.
    /// Filter unit is one of the 'all, merged, gone, local, remote, merged-local, merged-remote, gone-local, gone-remote'.
    /// 'all' implies 'merged-local,merged-remote,gone-local,gone-remote'.
    /// 'merged' implies 'merged-local,merged-remote'.
    /// 'gone' implies 'gone-local,gone-remote'.
    /// 'local' implies 'merged-local,gone-local'.
    /// 'remote' implies 'merged-remote,gone-remote'.
    ///
    /// You can scope a filter unit to specific remote ':<remote name>' to a 'filter unit'
    /// if the filter unit implies 'merged-remote' or 'gone-remote'.
    /// If there are filter units that is scoped, it trims merged or gone remote branches in the specified remote branch.
    /// If there are any filter unit that isn't scoped, it trims all merged or gone remote branches.
    /// [default : 'merged'] [config: trim.filter]
    #[structopt(short, long)]
    pub delete: Vec<DeleteFilter>,

    #[structopt(long)]
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
