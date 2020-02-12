use git2::Repository;

use fixture::{rc, Fixture};
use git_cleanup::{get_merged_or_gone, MergedOrGone};

pub mod fixture;

type Result<T> = ::std::result::Result<T, Error>;
type Error = Box<dyn std::error::Error>;

fn fixture() -> Fixture {
    rc().append_silent_fixture(
        r#"
        git init upstream
        upstream <<EOF
            git config user.name "UpstreamTest"
            git config user.email "upstream@test"
            echo "Hello World!" > README.md
            git add README.md
            git commit -m "Initial commit"
        EOF
        git clone upstream origin -o upstream
        origin <<EOF
            git config user.name "Origin Test"
            git config user.email "origin@test"
            git config remote.pushdefault upstream
            git config push.default current
        EOF
        git clone origin local
        local <<EOF
            git config user.name "Local Test"
            git config user.email "local@test"
            git config remote.pushdefault origin
            git config push.default current
            git remote add upstream ../upstream
            git fetch upstream
            git branch -u upstream/master master
        EOF
        # prepare awesome patch
        local <<EOF
            git checkout -b feature
            touch awesome-patch
            git add awesome-patch
            git commit -m "Awesome patch"
            git push -u origin
        EOF
        set -x
        "#,
    )
}

#[test]
fn test_accepted() -> Result<()> {
    let _guard = fixture().prepare(
        "local",
        r#"
        origin <<EOF
            git push upstream feature:refs/pull/1/head
        EOF
        upstream <<EOF
            git merge refs/pull/1/head
        EOF
        # clicked delete branch button
        origin <<EOF
            git branch -D feature
        EOF
        "#,
    )?;
    let repo = Repository::open_from_env()?;
    let branches = get_merged_or_gone(&repo, "master")?;
    assert_eq!(
        branches,
        MergedOrGone {
            merged_locals: set! {"feature"},
            ..Default::default()
        },
    );
    Ok(())
}
#[test]
fn test_accepted_but_edited() -> Result<()> {
    let _guard = fixture().prepare(
        "local",
        r#"
        origin <<EOF
            git push upstream feature:refs/pull/1/head
        EOF
        upstream <<EOF
            git merge refs/pull/1/head
        EOF
        # clicked delete branch button
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
    let repo = Repository::open_from_env()?;
    let branches = get_merged_or_gone(&repo, "master")?;
    assert_eq!(
        branches,
        MergedOrGone {
            gone_locals: set! {"feature"},
            ..Default::default()
        },
    );
    Ok(())
}
#[test]
fn test_accepted_but_forgot_to_delete() -> Result<()> {
    let _guard = fixture().prepare(
        "local",
        r#"
        origin <<EOF
            git push upstream feature:refs/pull/1/head
        EOF
        upstream <<EOF
            git merge refs/pull/1/head
        EOF
        "#,
    )?;
    let repo = Repository::open_from_env()?;
    let branches = get_merged_or_gone(&repo, "master")?;
    assert_eq!(
        branches,
        MergedOrGone {
            merged_locals: set! {"feature"},
            merged_remotes: set! {"refs/remotes/origin/feature"},
            ..Default::default()
        },
    );
    Ok(())
}
#[test]
fn test_accepted_but_forgot_to_delete_and_edited() -> Result<()> {
    let _guard = fixture().prepare(
        "local",
        r#"
        origin <<EOF
            git push upstream feature:refs/pull/1/head
        EOF
        upstream <<EOF
            git merge refs/pull/1/head
        EOF
        local <<EOF
            touch another-patch
            git add another-patch
            git commit -m "Another patch"
        EOF
        "#,
    )?;
    let repo = Repository::open_from_env()?;
    let branches = get_merged_or_gone(&repo, "master")?;
    assert_eq!(branches, MergedOrGone::default(),);
    Ok(())
}
#[test]
fn test_rejected() -> Result<()> {
    let _guard = fixture().prepare(
        "local",
        r#"
        origin <<EOF
            git push upstream feature:refs/pull/1/head
            git branch -D feature
        EOF
        "#,
    )?;
    let repo = Repository::open_from_env()?;
    let branches = get_merged_or_gone(&repo, "master")?;
    assert_eq!(
        branches,
        MergedOrGone {
            gone_locals: set! {"feature"},
            ..Default::default()
        },
    );
    Ok(())
}
#[test]
fn test_rejected_but_edited() -> Result<()> {
    let _guard = fixture().prepare(
        "local",
        r#"
        origin <<EOF
            git push upstream feature:refs/pull/1/head
            git branch -D feature
        EOF
        local <<EOF
            touch another-patch
            git add another-patch
            git commit -m "Another patch"
        EOF
        "#,
    )?;
    let repo = Repository::open_from_env()?;
    let branches = get_merged_or_gone(&repo, "master")?;
    assert_eq!(
        branches,
        MergedOrGone {
            gone_locals: set! {"feature"},
            ..Default::default()
        },
    );
    Ok(())
}
#[test]
fn test_rejected_but_forgot_to_delete() -> Result<()> {
    let _guard = fixture().prepare(
        "local",
        r#"
        origin <<EOF
            git push upstream feature:refs/pull/1/head
        EOF
        "#,
    )?;
    let repo = Repository::open_from_env()?;
    let branches = get_merged_or_gone(&repo, "master")?;
    assert_eq!(branches, MergedOrGone::default(),);
    Ok(())
}
#[test]
fn test_rejected_but_forgot_to_delete_and_edited() -> Result<()> {
    let _guard = fixture().prepare(
        "local",
        r#"
        origin <<EOF
            git push upstream feature:refs/pull/1/head
        EOF
        local <<EOF
            touch another-patch
            git add another-patch
            git commit -m "Another patch"
        EOF
        "#,
    )?;
    let repo = Repository::open_from_env()?;
    let branches = get_merged_or_gone(&repo, "master")?;
    assert_eq!(branches, MergedOrGone::default(),);
    Ok(())
}
