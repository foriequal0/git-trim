use std::str::FromStr;

use dialoguer::Confirmation;
use git2::Repository;
use log::*;

use git_trim::args::{Args, DeleteFilter};
use git_trim::{
    delete_local_branches, delete_remote_branches, get_config_bool, get_config_string,
    get_merged_or_gone, git,
};

type Result<T> = ::std::result::Result<T, Error>;
type Error = Box<dyn std::error::Error>;

#[paw::main]
fn main(args: Args) -> Result<()> {
    env_logger::init();
    info!("SEMVER: {}", env!("VERGEN_SEMVER"));
    info!("SHA: {}", env!("VERGEN_SHA"));
    info!("COMMIT_DATE: {}", env!("VERGEN_COMMIT_DATE"));
    info!("TARGET_TRIPLE: {}", env!("VERGEN_TARGET_TRIPLE"));

    let repo = Repository::open_from_env()?;

    let update = exclusive_flag(
        &repo,
        ("update", args.update),
        ("no-update", args.no_update),
        "trim.confirm",
        true,
    )?;

    let confirm = exclusive_flag(
        &repo,
        ("confirm", args.confirm),
        ("no-confirm", args.no_confirm),
        "trim.confirm",
        true,
    )?;

    let detach = exclusive_flag(
        &repo,
        ("detach", args.detach),
        ("no-detach", args.no_detach),
        "trim.detach",
        true,
    )?;

    let base = get_config_string(&repo, args.base, "trim.base", "master")?;

    let filter = if let Some(filter) = args.filter {
        filter
    } else {
        DeleteFilter::from_str(&get_config_string(&repo, None, "trim.delete", "merged")?)?
    };

    if !update {
        git(&["remote", "update", "--prune"])?;
    }
    let mut branches = get_merged_or_gone(&repo, &base)?;

    if detach {
        branches.adjust_not_to_detach(&repo)?;
    }

    branches.print_summary(&filter);

    let remote_refs_to_delete = branches.get_remote_refs_to_delete(&filter);
    let local_branches_to_delete = branches.get_local_branches_to_delete(&filter);
    let any_branches_to_remove =
        !(remote_refs_to_delete.is_empty() && local_branches_to_delete.is_empty());
    if !confirm
        && any_branches_to_remove
        && !Confirmation::new()
            .with_text("Confirm?")
            .default(false)
            .interact()?
    {
        println!("Cancelled");
        return Ok(());
    }

    delete_remote_branches(&repo, &remote_refs_to_delete, args.dry_run)?;
    delete_local_branches(&repo, &local_branches_to_delete, args.dry_run)?;
    Ok(())
}

fn exclusive_flag(
    repo: &Repository,
    (name_pos, value_pos): (&str, bool),
    (name_neg, value_neg): (&str, bool),
    key: &str,
    default: bool,
) -> Result<bool> {
    if value_pos == value_neg {
        return Err(format!(
            "Flag '{}' and '{}' cannot be used simultinusly",
            name_pos, name_neg
        )
        .into());
    };
    let flag = if value_pos {
        Some(true)
    } else if value_neg {
        Some(false)
    } else {
        None
    };
    get_config_bool(repo, flag, key, default)
}
