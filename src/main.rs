use dialoguer::Confirmation;
use git2::Repository;
use log::*;

use git_cleanup::args::Args;
use git_cleanup::{
    delete_local_branches, delete_remote_branches, get_config_base, get_merged_or_gone, git,
};

#[paw::main]
fn main(args: Args) -> ::std::result::Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    info!("SEMVER: {}", env!("VERGEN_SEMVER"));
    info!("SHA: {}", env!("VERGEN_SHA"));
    info!("COMMIT_DATE: {}", env!("VERGEN_COMMIT_DATE"));
    info!("TARGET_TRIPLE: {}", env!("VERGEN_TARGET_TRIPLE"));

    if !args.no_update {
        git(&["remote", "update", "--prune"])?;
    }

    let repo = Repository::open_from_env()?;
    let base = get_config_base(&repo, args.base)?;
    let mut branches = get_merged_or_gone(&repo, &base)?;

    if args.no_detach {
        branches.adjust_not_to_detach(&repo)?;
    }

    branches.print_summary(&args.delete);

    if !args.no_confirm
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
        &branches.get_remote_refs_to_delete(&args.delete),
        args.dry_run,
    )?;
    delete_local_branches(
        &repo,
        &branches.get_local_branches_to_delete(&args.delete),
        args.dry_run,
    )?;
    Ok(())
}
