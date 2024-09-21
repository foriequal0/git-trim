mod fixture;

use std::convert::TryFrom;

use anyhow::Result;
use git2::Repository;

use git_trim::{get_trim_plan, ClassifiedBranch, Git, LocalBranch, RemoteTrackingBranch};

use fixture::{rc, test_default_param, Fixture};

fn fixture() -> Fixture {
    rc().append_fixture_trace(
        r#"
        git init origin
        origin <<EOF
            git config user.name "Origin Test"
            git config user.email "origin@test"
            echo "Hello World!" > README.md
            git add README.md
            git commit -m "Initial commit"
        EOF
        git clone origin local
        local <<EOF
            git config user.name "Local Test"
            git config user.email "local@test"
            git config remote.pushdefault origin
            git config push.default simple
        EOF
        # prepare awesome patch
        local <<EOF
            git checkout -b feature
            touch awesome-patch
            git add awesome-patch
            git commit -m "Awesome patch"
            git push -u origin feature
        EOF
        "#,
    )
}

#[test]
fn test_accepted() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r"
        origin <<EOF
            git checkout master
            git merge feature
            git branch -d feature
        EOF
        ",
    )?;

    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let plan = get_trim_plan(&git, &test_default_param())?;
    assert_eq!(
        plan.to_delete,
        set! {
            ClassifiedBranch::MergedLocal(LocalBranch::new("refs/heads/feature")),
        },
    );
    Ok(())
}

#[test]
fn test_accepted_but_edited() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        origin <<EOF
            git checkout master
            git merge feature
            git branch -d feature
        EOF
        local <<EOF
            touch another-patch
            git add another-patch
            git commit -m "Another patch"
        EOF
        "#,
    )?;
    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let plan = get_trim_plan(&git, &test_default_param())?;
    assert_eq!(
        plan.to_delete,
        set! {
            ClassifiedBranch::Stray(LocalBranch::new("refs/heads/feature")),
        },
    );
    Ok(())
}

#[test]
fn test_accepted_but_forgot_to_delete() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r"
        origin <<EOF
            git checkout master
            git merge feature
        EOF
        ",
    )?;
    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let plan = get_trim_plan(&git, &test_default_param())?;
    assert_eq!(
        plan.to_delete,
        set! {
            ClassifiedBranch::MergedLocal(LocalBranch::new("refs/heads/feature")),
            ClassifiedBranch::MergedRemoteTracking(RemoteTrackingBranch::new("refs/remotes/origin/feature")),
        },
    );
    Ok(())
}

#[test]
fn test_accepted_but_forgot_to_delete_and_edited() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        origin <<EOF
            git checkout master
            git merge feature
        EOF
        local <<EOF
            touch another-patch
            git add another-patch
            git commit -m "Another patch"
        EOF
        "#,
    )?;
    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let plan = get_trim_plan(&git, &test_default_param())?;
    assert_eq!(
        plan.to_delete,
        set! {
            ClassifiedBranch::Stray(LocalBranch::new("refs/heads/feature")),
            ClassifiedBranch::MergedRemoteTracking(RemoteTrackingBranch::new("refs/remotes/origin/feature")),
        },
    );
    Ok(())
}

#[test]
fn test_rejected() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r"
        origin <<EOF
            git branch -D feature
        EOF
    ",
    )?;
    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let plan = get_trim_plan(&git, &test_default_param())?;
    assert_eq!(
        plan.to_delete,
        set! {
            ClassifiedBranch::Stray(LocalBranch::new("refs/heads/feature")),
        },
    );
    Ok(())
}

#[test]
fn test_rejected_but_edited() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        origin <<EOF
            git branch -D feature
        EOF
        local <<EOF
            touch another-patch
            git add another-patch
            git commit -m "Another patch"
        EOF
    "#,
    )?;
    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let plan = get_trim_plan(&git, &test_default_param())?;
    assert_eq!(
        plan.to_delete,
        set! {
            ClassifiedBranch::Stray(LocalBranch::new("refs/heads/feature")),
        },
    );
    Ok(())
}

#[test]
fn test_rejected_but_forgot_to_delete() -> Result<()> {
    let guard = fixture().prepare("local", r"")?;
    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let plan = get_trim_plan(&git, &test_default_param())?;
    assert_eq!(plan.to_delete, set! {});
    Ok(())
}

#[test]
fn test_rejected_but_forgot_to_delete_and_edited() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        local <<EOF
            touch another-patch
            git add another-patch
            git commit -m "Another patch"
        EOF
    "#,
    )?;
    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let plan = get_trim_plan(&git, &test_default_param())?;
    assert_eq!(plan.to_delete, set! {});
    Ok(())
}
