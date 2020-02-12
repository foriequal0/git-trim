use git2::Repository;
use git_cleanup::{
    delete_local_branches, delete_remote_branches, get_config_base, get_merged_or_gone, git,
};
use log::*;

use dialoguer::Confirmation;
use git_cleanup::args::Args;

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
    let branches = get_merged_or_gone(&repo, &base)?;

    branches.print_summary(&args.delete);

    if !Confirmation::new()
        .with_text("Confirm?")
        .default(false)
        .interact()?
    {
        println!("Cancelled");
        return Ok(());
    }

    delete_local_branches(
        &branches.get_local_branches_to_delete(&args.delete),
        args.dry_run,
    )?;
    delete_remote_branches(
        &repo,
        &branches.get_remote_refs_to_delete(&args.delete),
        args.dry_run,
    )?;
    Ok(())
}
