use std::collections::HashSet;
use std::fmt::{Debug, Display, Formatter};
use std::hash::Hash;
use std::iter::FromIterator;
use std::process::exit;
use std::str::FromStr;

#[derive(Hash, Eq, PartialEq, Copy, Clone, Debug)]
pub enum Category {
    MergedLocal,
    MergedRemote,
    GoneLocal,
    GoneRemote,
}

#[derive(Debug, Clone)]
pub struct DeleteFilter(HashSet<Category>);

impl DeleteFilter {
    pub fn merged() -> Self {
        use Category::*;
        DeleteFilter::from_iter(vec![MergedLocal, MergedRemote])
    }

    pub fn all() -> Self {
        use Category::*;
        DeleteFilter::from_iter(vec![MergedLocal, MergedRemote, GoneLocal, GoneRemote])
    }

    pub fn filter_merged_local(&self) -> bool {
        self.0.contains(&Category::MergedLocal)
    }

    pub fn filter_merged_remote(&self) -> bool {
        for filter in self.0.iter() {
            if let Category::MergedRemote = filter {
                return true;
            }
        }
        false
    }

    pub fn filter_gone_local(&self) -> bool {
        self.0.contains(&Category::GoneLocal)
    }

    pub fn filter_gone_remote(&self) -> bool {
        for filter in self.0.iter() {
            if let Category::GoneRemote = filter {
                return true;
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
        use Category::*;
        let mut result = HashSet::new();
        for arg in args.split(',') {
            let x: &[_] = match arg.trim() {
                "all" => &[MergedLocal, MergedRemote, GoneLocal, GoneRemote],
                "merged" => &[MergedLocal, MergedRemote],
                "gone" => &[GoneLocal, GoneRemote],
                "local" => &[MergedLocal, GoneLocal],
                "remote" => &[MergedRemote, GoneRemote],
                "merged-local" => &[MergedLocal],
                "merged-remote" => &[MergedRemote],
                "gone-local" => &[GoneLocal],
                "gone-remote" => &[GoneRemote],
                _ if arg.is_empty() => &[],
                _ => {
                    return Err(DeleteFilterParseError {
                        message: format!("Unexpected branch category: {}", arg),
                    });
                }
            };
            result.extend(x.iter().copied());
        }

        Ok(DeleteFilter(result))
    }
}

impl FromIterator<Category> for DeleteFilter {
    fn from_iter<T>(iter: T) -> Self
    where
        T: IntoIterator<Item = Category>,
    {
        Self(HashSet::from_iter(iter))
    }
}

impl IntoIterator for DeleteFilter {
    type Item = Category;
    type IntoIter = std::collections::hash_set::IntoIter<Category>;

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

    /// Comma separated values of [all, merged, gone, local, remote, merged-local, merged-remote, gone-local, gone-remote].
    /// 'all' is equivalent to 'merged-local,merged-remote,gone-local,gone-remote'.
    /// 'merged' is equivalent to 'merged-local,merged-remote'.
    /// 'gone' is equivalent to 'gone-local,gone-remote'.
    /// 'local' is equivalent to 'merged-local,gone-local'.
    /// 'remote' is equivalent to 'merged-remote,gone-remote'. [default : 'merged'] [config: trim.filter]
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
