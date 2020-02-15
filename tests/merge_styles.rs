extern crate git_trim;

use git2::Repository;

use fixture::{rc, Fixture};
use git_trim::{get_merged_or_gone, MergedOrGone};

mod fixture;

type Result<T> = ::std::result::Result<T, Error>;
type Error = Box<dyn std::error::Error>;

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
            git config push.default current
        EOF
        # prepare awesome patch
        local <<EOF
            git checkout -b feature
            touch awesome-patch
            git add awesome-patch
            git commit -m "Awesome patch"
            touch another-patch
            git add another-patch
            git commit -m "Another patch"
            git push -u origin
        EOF
        "#,
    )
}

#[test]
fn test_noff() -> Result<()> {
    let _guard = fixture().prepare(
        "local",
        r#"
        origin <<EOF
            git checkout master
            git merge feature --no-ff
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
fn test_rebase() -> Result<()> {
    let _guard = fixture().prepare(
        "local",
        r#"
        origin <<EOF
            git checkout -b rebase-tmp feature
            git rebase master
            git checkout master
            git merge rebase-tmp --ff-only
            git branch -D rebase-tmp feature
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
fn test_squash() -> Result<()> {
    let _guard = fixture().prepare(
        "local",
        r#"
        origin <<EOF
            git checkout master
            git merge feature --squash && git commit --no-edit
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
