use std::str::FromStr;

use dialoguer::Confirmation;
use git2::Repository;
use log::*;

use git_trim::args::{Args, DeleteFilter};
use git_trim::{
    delete_local_branches, delete_remote_branches, get_config_bool, get_config_string,
    get_merged_or_gone, git, ConfigValue,
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

    let base = get_config_string(&repo, args.base.as_ref(), "trim.base", "master")?;
    let update = get_config_bool(&repo, args.update(), "trim.update", true)?;
    let confirm = get_config_bool(&repo, args.confirm(), "trim.confirm", true)?;
    let detach = get_config_bool(&repo, args.detach(), "trim.detach", true)?;
    let filter = if let Some(filter) = args.filter {
        ConfigValue::Explicit {
            value: filter,
            source: "cli".to_string(),
        }
    } else {
        get_config_string(&repo, None, "trim.filter", "merged")?
            .map(|s| DeleteFilter::from_str(s).unwrap())
    };

    info!("base: {:?}", base);
    info!("update: {:?}", update);
    info!("confirm: {:?}", confirm);
    info!("detach: {:?}", detach);
    info!("filter: {:?}", filter);

    if *update {
        git(&["remote", "update", "--prune"])?;
    }
    let mut branches = get_merged_or_gone(&repo, &base)?;

    if *detach {
        branches.adjust_not_to_detach(&repo)?;
    }

    branches.print_summary(&filter);

    if *confirm
        && branches.are_deleted(&filter)
        && !Confirmation::new()
            .with_text("Confirm?")
            .default(false)
            .interact()?
    {
        println!("Cancelled");
        return Ok(());
    }

    delete_remote_branches(
        &repo,
        &branches.get_remote_refs_to_delete(&filter),
        args.dry_run,
    )?;
    delete_local_branches(
        &repo,
        &branches.get_merged_locals(&filter),
        false,
        args.dry_run,
    )?;
    delete_local_branches(
        &repo,
        &branches.get_gone_locals(&filter),
        true,
        args.dry_run,
    )?;

    Ok(())
}
