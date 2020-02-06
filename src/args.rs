use std::collections::HashSet;
use std::fmt::{Display, Error, Formatter};
use std::str::FromStr;

#[derive(structopt::StructOpt, Hash, Eq, PartialEq, Copy, Clone)]
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

#[derive(derive_deref::Deref)]
pub struct DeleteFilter(HashSet<Category>);

impl FromStr for DeleteFilter {
    type Err = String;

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
                _ => return Err(format!("Unexpected branch category: {}", arg)),
            };
            result.extend(x.iter().copied());
        }

        Ok(DeleteFilter(result))
    }
}

#[derive(structopt::StructOpt)]
pub struct Args {
    /// A ref that other refs are compared to determine whether it is merged or gone.
    #[structopt(short, long)]
    pub base: Option<String>,

    #[structopt(short, long)]
    pub no_update: bool,

    /// Comma separated values of [all, merged, gone, local, remote, merged-local, merged-remote, gone-local, gone-remote].
    /// 'all' is equivalent to 'merged-local,merged-remote,gone-local,gone-remote'.
    /// 'merged' is equivalent to 'merged-local,merged-remote'.
    /// 'gone' is equivalent to 'gone-local,gone-remote'.
    /// 'local' is equivalent to 'merged-local,gone-local'.
    /// 'remote' is equivalent to 'merged-remote,gone-remote'.
    #[structopt(short, long, parse(try_from_str), default_value = "merged")]
    pub delete: DeleteFilter,

    #[structopt(long)]
    pub dry_run: bool,
}
