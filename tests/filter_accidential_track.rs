mod fixture;

use std::convert::TryFrom;
use std::iter::FromIterator;

use anyhow::Result;
use git2::Repository;

use git_trim::args::{DeleteFilter, FilterUnit, Scope};
use git_trim::{
    get_trim_plan, ClassifiedBranch, Git, LocalBranch, PlanParam, RemoteTrackingBranch,
};

use fixture::{rc, Fixture};

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
        EOF

        local <<EOF
            git remote add contributer ../contributer
            git remote update
        EOF
        "#,
    )
}

fn param() -> PlanParam<'static> {
    PlanParam {
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
    let plan = get_trim_plan(
        &git,
        &PlanParam {
            filter: DeleteFilter::all(),
            ..param()
        },
    )?;
    assert_eq!(
        plan.to_delete,
        set! {
            ClassifiedBranch::MergedLocal(LocalBranch::new("refs/heads/feature")),
            ClassifiedBranch::MergedRemoteTracking(
                RemoteTrackingBranch::new("refs/remotes/contributer/feature")),
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
    let plan = get_trim_plan(&git, &param())?;
    assert_eq!(
        plan.to_delete,
        set! {
            ClassifiedBranch::MergedLocal(LocalBranch::new("refs/heads/feature")),
        },
    );
    Ok(())
}
