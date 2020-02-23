use std::collections::HashSet;
use std::fmt::{Debug, Display, Error, Formatter};
use std::hash::Hash;
use std::iter::FromIterator;
use std::process::exit;
use std::str::FromStr;

#[derive(structopt::StructOpt, Hash, Eq, PartialEq, Copy, Clone, Debug)]
pub enum Category {
    MergedLocal,
    MergedRemote,
    GoneLocal,
    GoneRemote,
}

impl Display for Category {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), Error> {
        match self {
            Category::MergedLocal => write!(f, "merged local branch"),
            Category::MergedRemote => write!(f, "merged remote ref"),
            Category::GoneLocal => write!(f, "gone local branch"),
            Category::GoneRemote => write!(f, "gone_remote ref"),
        }
    }
}

#[derive(derive_deref::Deref, Debug, Clone)]
pub struct DeleteFilter(HashSet<Category>);

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

impl Default for DeleteFilter {
    fn default() -> Self {
        use Category::*;
        DeleteFilter(vec![MergedLocal, MergedRemote].into_iter().collect())
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
pub struct CommaSeparatedUniqueVec<T: Debug + Eq>(Vec<T>);

impl<T> FromStr for CommaSeparatedUniqueVec<T>
where
    T: FromStr + Clone + Debug + Eq,
{
    type Err = T::Err;

    fn from_str(args: &str) -> Result<Self, Self::Err> {
        let mut result = Vec::new();
        for arg in args.split(',') {
            let parsed = arg.trim().parse()?;
            if !result.contains(&parsed) {
                result.push(parsed);
            }
        }
        Ok(Self(result))
    }
}

impl<T> FromIterator<T> for CommaSeparatedUniqueVec<T>
where
    T: Default + Debug + Eq,
{
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut result = Vec::new();
        for item in iter.into_iter() {
            if !result.contains(&item) {
                result.push(item);
            }
        }
        Self(result)
    }
}

impl<T> CommaSeparatedUniqueVec<T>
where
    T: Default + Debug + Eq + Hash,
{
    fn accumulate(mut self, other: Self) -> Self {
        for item in other.0.into_iter() {
            if !self.0.contains(&item) {
                self.0.push(item);
            }
        }
        Self(self.0)
    }

    pub fn flatten<I>(args: I) -> Option<Self>
    where
        I: IntoIterator<Item = Self>,
    {
        let result = args.into_iter().fold(Self::default(), Self::accumulate);
        if result.len() == 0 {
            None
        } else {
            Some(result)
        }
    }
}

#[derive(derive_deref::Deref, Debug, Clone, Default)]
pub struct CommaSeparatedSet<T: Debug + Eq + Hash>(HashSet<T>);

impl<T> FromStr for CommaSeparatedSet<T>
where
    T: FromStr + Clone + Debug + Eq + Hash,
{
    type Err = T::Err;

    fn from_str(args: &str) -> Result<Self, Self::Err> {
        let mut result = HashSet::new();
        for arg in args.split(',') {
            result.insert(arg.trim().parse()?);
        }
        Ok(Self(result))
    }
}

impl<T> FromIterator<T> for CommaSeparatedSet<T>
where
    T: Debug + Eq + Hash,
{
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut result = HashSet::new();
        for item in iter.into_iter() {
            result.insert(item);
        }
        Self(result)
    }
}

impl<T> CommaSeparatedSet<T>
where
    T: Default + Debug + Eq + Hash,
{
    fn accumulate(self, other: Self) -> Self {
        Self(self.0.into_iter().chain(other.0.into_iter()).collect())
    }

    pub fn flatten<I>(args: I) -> Option<Self>
    where
        I: IntoIterator<Item = Self>,
    {
        let result = args.into_iter().fold(Self::default(), Self::accumulate);
        if result.len() == 0 {
            None
        } else {
            Some(result)
        }
    }
}

#[derive(structopt::StructOpt)]
pub struct Args {
    /// Comma separated or a multiple arguments of refs that other refs are compared to determine whether it is merged or gone. [default: master] [config: trim.base]
    #[structopt(short, long, aliases=&["base"])]
    pub bases: Vec<CommaSeparatedUniqueVec<String>>,

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
    #[structopt(short, long, parse(try_from_str))]
    pub delete: Option<DeleteFilter>,

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
