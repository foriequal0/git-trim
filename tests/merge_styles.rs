mod fixture;

use std::convert::TryFrom;

use anyhow::Result;
use git2::Repository;

use git_trim::{get_merged_or_gone, Config, Git, MergedOrGone};

use fixture::{rc, Fixture};

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
            touch another-patch
            git add another-patch
            git commit -m "Another patch"
            git push -u origin feature
        EOF
        "#,
    )
}

fn config() -> Config<'static> {
    Config {
        bases: vec!["master"],
        protected_branches: set! {},
        detach: true,
    }
}

#[test]
fn test_noff() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        origin <<EOF
            git checkout master
            git merge feature --no-ff
            git branch -D feature
        EOF
        "#,
    )?;

    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let branches = get_merged_or_gone(&git, &config())?;
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
    let guard = fixture().prepare(
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

    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let branches = get_merged_or_gone(&git, &config())?;
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
    let guard = fixture().prepare(
        "local",
        r#"
        origin <<EOF
            git checkout master
            git merge feature --squash && git commit --no-edit
            git branch -D feature
        EOF
        "#,
    )?;

    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let branches = get_merged_or_gone(&git, &config())?;
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
fn test_mixed() -> Result<()> {
    let fixture = rc().append_fixture_trace(
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
        mk_test_branches() {
            BASE=$1
            NAME=$2
            COUNT=$3
            git checkout -b "$NAME" "$BASE"
            for i in $(seq "$COUNT"); do
                touch "$NAME-$i"
                git add "$NAME-$i"
                git commit -m "Add $NAME-$i"
            done
            git push -u origin "$NAME"
        }
        local <<EOF
            mk_test_branches master rebaseme 3
            mk_test_branches master squashme 3
            mk_test_branches master noffme 3
        EOF
        "#,
    );
    let guard = fixture.prepare(
        "local",
        r#"
        origin <<EOF
            # squash
            git checkout master
            git merge squashme --squash && git commit --no-edit
            git branch -D squashme

            # rebaseme
            git checkout -b rebase-tmp rebaseme
            git rebase master
            git checkout master
            git merge rebase-tmp --ff-only
            git branch -D rebase-tmp rebaseme

            # noff
            git checkout master
            git merge noffme --no-ff
            git branch -D noffme
        EOF
        "#,
    )?;

    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let branches = get_merged_or_gone(&git, &config())?;
    assert_eq!(
        branches,
        MergedOrGone {
            merged_locals: set! {"squashme", "rebaseme", "noffme"},
            ..Default::default()
        },
    );
    Ok(())
}
