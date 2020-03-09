mod fixture;

use std::convert::TryFrom;

use anyhow::Result;
use git2::Repository;

use git_trim::args::{DeleteFilter, FilterUnit, Scope};
use git_trim::{get_merged_or_gone, Config, Git, MergedOrGone, RemoteBranch};

use fixture::{rc, Fixture};
use std::iter::FromIterator;

fn fixture() -> Fixture {
    rc().append_fixture_trace(
        r#"
        git init origin --bare

        git clone origin local
        local <<EOF
            git config user.name "Local Test"
            git config user.email "local@test"
            git config remote.pushdefault origin
            git config push.default simple

            echo "Hello World!" > README.md
            git add README.md
            git commit -m "Initial commit"
            git push -u origin master
        EOF

        git clone origin contributer
        within contributer <<EOF
            git config user.name "Contributer Test"
            git config user.email "contributer@test"
            git config remote.pushdefault origin
            git config push.default simple
        EOF

        within contributer <<EOF
            git checkout -b feature
            touch awesome-patch
            git add awesome-patch
            git commit -m "Awesome patch"
            touch another-patch
            git add another-patch
            git commit -m "Another patch"
            git push -u origin feature
        EOF

        local <<EOF
            git remote add contributer ../contributer
            git remote update
        EOF
        "#,
    )
}

fn config() -> Config<'static> {
    Config {
        bases: vec!["master"],
        protected_branches: set! {},
        filter: DeleteFilter::from_iter(vec![
            FilterUnit::MergedLocal,
            FilterUnit::MergedRemote(Scope::Scoped("origin".to_string())),
        ]),
        detach: true,
    }
}

#[test]
fn test_default_config_tries_to_delete_accidential_track() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        local <<EOF
            git checkout --track contributer/feature

            git checkout master
            git merge feature --no-ff
            git push -u origin master
        EOF
        "#,
    )?;

    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let branches = get_merged_or_gone(
        &git,
        &Config {
            filter: DeleteFilter::all(),
            ..config()
        },
    )?;
    assert_eq!(
        branches.to_delete,
        MergedOrGone {
            merged_locals: set! {"feature"},
            merged_remotes: set! {
                RemoteBranch {
                    remote: "contributer".to_string(),
                    refname: "refs/heads/feature".to_string()
                },
            },
            ..Default::default()
        },
    );
    Ok(())
}

#[test]
fn test_accidential_track() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        local <<EOF
            git checkout --track contributer/feature

            git checkout master
            git merge feature --no-ff
            git push -u origin master
        EOF
        "#,
    )?;

    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let branches = get_merged_or_gone(&git, &config())?;
    assert_eq!(
        branches.to_delete,
        MergedOrGone {
            merged_locals: set! {"feature"},
            ..Default::default()
        },
    );
    Ok(())
}
