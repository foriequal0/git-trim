mod fixture;

use std::convert::TryFrom;
use std::iter::FromIterator;

use anyhow::Result;
use git2::Repository;

use git_trim::args::{DeleteFilter, DeleteRange, Scope};
use git_trim::{
    get_trim_plan, ClassifiedBranch, Git, LocalBranch, PlanParam, RemoteTrackingBranch,
};

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
            git push origin feature
        EOF
        "#,
    )
}

fn param() -> PlanParam<'static> {
    PlanParam {
        delete: DeleteFilter::from_iter(vec![
            DeleteRange::MergedLocal,
            DeleteRange::MergedRemote(Scope::Scoped("origin".to_owned())),
            DeleteRange::Stray,
            DeleteRange::Diverged(Scope::Scoped("origin".to_owned())),
            DeleteRange::Local,
            DeleteRange::Remote(Scope::Scoped("origin".to_owned())),
        ]),
        ..test_default_param()
    }
}

#[test]
fn test_merged_non_tracking() -> Result<()> {
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
    let plan = get_trim_plan(&git, &param())?;
    assert_eq!(
        plan.to_delete,
        set! {
            ClassifiedBranch::MergedNonTrackingLocal(LocalBranch::new("refs/heads/feature")),
        },
    );
    Ok(())
}

#[test]
fn test_merged_non_upstream() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r"
        origin <<EOF
            git config core.bare true
        EOF
        local <<EOF
            git checkout master
            git merge feature
            git branch -D feature
            git push origin master
        EOF
        ",
    )?;

    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let plan = get_trim_plan(&git, &param())?;
    assert_eq!(
        plan.to_delete,
        set! {
            ClassifiedBranch::MergedNonUpstreamRemoteTracking(RemoteTrackingBranch::new("refs/remotes/origin/feature")),
        },
    );
    Ok(())
}
