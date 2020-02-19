use std::iter::FromIterator;

use dialoguer::Confirmation;
use git2::Repository;
use log::*;

use git_trim::args::{Args, CommaSeparatedUniqueVec, DeleteFilter};
use git_trim::config;
use git_trim::{delete_local_branches, delete_remote_branches, get_merged_or_gone, remote_update};

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
    let config = repo.config()?.snapshot()?;

    let bases = config::get(&config, "trim.bases")
        .with_explicit("cli", CommaSeparatedUniqueVec::flatten(args.bases.clone()))
        .with_default(&CommaSeparatedUniqueVec::from_iter(vec![
            String::from("develop"),
            String::from("master"),
        ]))
        .parse()?
        .expect("has default");
    let update = config::get(&config, "trim.update")
        .with_explicit("cli", args.update())
        .with_default(&true)
        .read()?
        .expect("has default");
    let confirm = config::get(&config, "trim.confirm")
        .with_explicit("cli", args.confirm())
        .with_default(&true)
        .read()?
        .expect("has default");
    let detach = config::get(&config, "trim.detach")
        .with_explicit("cli", args.detach())
        .with_default(&true)
        .read()?
        .expect("has default");
    let filter = config::get(&config, "trim.delete")
        .with_explicit("cli", args.delete)
        .with_default(&DeleteFilter::default())
        .parse()?
        .expect("has default");

    info!("bases: {:?}", bases);
    info!("update: {:?}", update);
    info!("confirm: {:?}", confirm);
    info!("detach: {:?}", detach);
    info!("filter: {:?}", filter);

    if *update {
        remote_update(&repo)?;
    }

    let mut branches = get_merged_or_gone(&repo, &config, &bases)?;

    branches.keep_base(&repo, &config, &bases)?;
    if *detach {
        branches.adjust_not_to_detach(&repo)?;
    }

    branches.print_summary(&filter);

    let remote_refs_to_delete = branches.get_remote_refs_to_delete(&filter);
    let local_branches_to_delete = branches.get_local_branches_to_delete(&filter);
    let any_branches_to_remove =
        !(remote_refs_to_delete.is_empty() && local_branches_to_delete.is_empty());
    if !args.dry_run
        && *confirm
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
