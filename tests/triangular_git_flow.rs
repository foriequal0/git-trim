mod fixture;

use std::convert::TryFrom;

use anyhow::Result;
use git2::Repository;

use git_trim::{
    get_trim_plan, ClassifiedBranch, Git, LocalBranch, PlanParam, RemoteTrackingBranch,
};

use fixture::{rc, test_default_param, Fixture};

fn fixture() -> Fixture {
    rc().append_fixture_trace(
        r#"
        git init upstream
        upstream <<EOF
            git config user.name "UpstreamTest"
            git config user.email "upstream@test"
            echo "Hello World!" > README.md
            git add README.md
            git commit -m "Initial commit"
            git branch develop main
        EOF
        git clone upstream origin -o upstream
        origin <<EOF
            git config user.name "Origin Test"
            git config user.email "origin@test"
            git config remote.pushdefault upstream
            git pull --all
        EOF
        git clone origin local
        local <<EOF
            git config user.name "Local Test"
            git config user.email "local@test"
            git config remote.pushdefault origin
            git config push.default simple
            git remote add upstream ../upstream
            git fetch upstream
            git branch -u upstream/main main
            git branch develop upstream/develop
            git branch -u upstream/develop develop

            git checkout develop
        EOF
        "#,
    )
}

fn param() -> PlanParam<'static> {
    PlanParam {
        bases: vec!["develop", "main"], // Need to set bases manually for git flow
        ..test_default_param()
    }
}

#[test]
fn test_feature_to_develop() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        local <<EOF
            git checkout -b feature
            touch awesome-patch
            git add awesome-patch
            git commit -m "Awesome patch"
            git push -u origin feature
        EOF

        # Create PR
        origin <<EOF
            git push upstream feature:refs/pull/1/head
        EOF

        upstream <<EOF
            git checkout develop
            git merge refs/pull/1/head
        EOF

        # clicked delete branch button
        origin <<EOF
            git branch -D feature
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

#[test]
fn test_feature_to_develop_but_forgot_to_delete() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        local <<EOF
            git checkout -b feature
            touch awesome-patch
            git add awesome-patch
            git commit -m "Awesome patch"
            git push -u origin feature
        EOF

        # Create PR
        origin <<EOF
            git push upstream feature:refs/pull/1/head
        EOF

        upstream <<EOF
            git checkout develop
            git merge refs/pull/1/head
        EOF
        "#,
    )?;

    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let plan = get_trim_plan(&git, &param())?;

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
fn test_develop_to_main() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        local <<EOF
            git checkout -b feature
            touch awesome-patch
            git add awesome-patch
            git commit -m "Awesome patch"
            git push -u origin feature
        EOF

        # Create PR
        origin <<EOF
            git push upstream feature:refs/pull/1/head
        EOF

        upstream <<EOF
            git checkout develop
            git merge refs/pull/1/head

            git checkout main
            git merge develop
        EOF

        # clicked delete branch button
        origin <<EOF
            git branch -D feature
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

#[test]
fn test_develop_to_main_but_forgot_to_delete() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        local <<EOF
            git checkout -b feature
            touch awesome-patch
            git add awesome-patch
            git commit -m "Awesome patch"
            git push -u origin feature
        EOF

        # Create PR
        origin <<EOF
            git push upstream feature:refs/pull/1/head
        EOF

        upstream <<EOF
            git checkout develop
            git merge refs/pull/1/head

            git checkout main
            git merge develop
        EOF
        "#,
    )?;

    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let plan = get_trim_plan(&git, &param())?;

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
fn test_hotfix_to_main() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        # prepare awesome patch
        local <<EOF
            git checkout main
            git checkout -b hotfix
            touch hotfix
            git add hotfix
            git commit -m "Hotfix"
            git push -u origin hotfix
        EOF

        # Create PR
        origin <<EOF
            git push upstream hotfix:refs/pull/1/head
        EOF

        upstream <<EOF
            git checkout main
            git merge refs/pull/1/head
        EOF

        # clicked delete branch button
        origin <<EOF
            git branch -D hotfix
        EOF
        "#,
    )?;

    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let plan = get_trim_plan(&git, &param())?;

    assert_eq!(
        plan.to_delete,
        set! {
            ClassifiedBranch::MergedLocal(LocalBranch::new("refs/heads/hotfix")),
        },
    );
    Ok(())
}

#[test]
fn test_hotfix_to_main_forgot_to_delete() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        # prepare awesome patch
        local <<EOF
            git checkout main
            git checkout -b hotfix
            touch hotfix
            git add hotfix
            git commit -m "Hotfix"
            git push -u origin hotfix
        EOF

        # Create PR
        origin <<EOF
            git push upstream hotfix:refs/pull/1/head
        EOF

        upstream <<EOF
            git checkout main
            git merge refs/pull/1/head
        EOF
        "#,
    )?;

    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let plan = get_trim_plan(&git, &param())?;

    assert_eq!(
        plan.to_delete,
        set! {
            ClassifiedBranch::MergedLocal(LocalBranch::new("refs/heads/hotfix")),
            ClassifiedBranch::MergedRemoteTracking(RemoteTrackingBranch::new("refs/remotes/origin/hotfix")),
        },
    );
    Ok(())
}

#[test]
fn test_rejected_feature_to_develop() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        local <<EOF
            git checkout -b feature
            touch awesome-patch
            git add awesome-patch
            git commit -m "Awesome patch"
            git push -u origin feature
        EOF

        # Create PR
        origin <<EOF
            git push upstream feature:refs/pull/1/head
        EOF

        # clicked delete branch button
        origin <<EOF
            git branch -D feature
        EOF
        "#,
    )?;

    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let plan = get_trim_plan(&git, &param())?;

    assert_eq!(
        plan.to_delete,
        set! {
            ClassifiedBranch::Stray(LocalBranch::new("refs/heads/feature")),
        },
    );
    Ok(())
}

#[test]
fn test_rejected_hotfix_to_main() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        # prepare awesome patch
        local <<EOF
            git checkout main
            git checkout -b hotfix
            touch hotfix
            git add hotfix
            git commit -m "Hotfix"
            git push -u origin hotfix
        EOF

        # Create PR
        origin <<EOF
            git push upstream hotfix:refs/pull/1/head
        EOF

        # clicked delete branch button
        origin <<EOF
            git branch -D hotfix
        EOF
        "#,
    )?;

    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let plan = get_trim_plan(&git, &param())?;

    assert_eq!(
        plan.to_delete,
        set! {
            ClassifiedBranch::Stray(LocalBranch::new("refs/heads/hotfix")),
        },
    );
    Ok(())
}

#[test]
fn test_protected_feature_to_develop() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        local <<EOF
            git checkout -b feature
            touch awesome-patch
            git add awesome-patch
            git commit -m "Awesome patch"
            git push -u origin feature
        EOF

        # Create PR
        origin <<EOF
            git push upstream feature:refs/pull/1/head
        EOF

        upstream <<EOF
            git checkout develop
            git merge refs/pull/1/head
        EOF

        # clicked delete branch button
        origin <<EOF
            git branch -D feature
        EOF
        "#,
    )?;

    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let plan = get_trim_plan(
        &git,
        &PlanParam {
            protected_patterns: vec!["refs/heads/feature"],
            ..param()
        },
    )?;

    assert_eq!(plan.to_delete, set! {});
    Ok(())
}

#[test]
fn test_protected_feature_to_main() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        local <<EOF
            git checkout -b feature
            touch awesome-patch
            git add awesome-patch
            git commit -m "Awesome patch"
            git push -u origin feature
        EOF

        # Create PR
        origin <<EOF
            git push upstream feature:refs/pull/1/head
        EOF

        upstream <<EOF
            git checkout develop
            git merge refs/pull/1/head

            git checkout main
            git merge develop
        EOF

        # clicked delete branch button
        origin <<EOF
            git branch -D feature
        EOF
        "#,
    )?;

    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let plan = get_trim_plan(
        &git,
        &PlanParam {
            protected_patterns: vec!["refs/heads/feature"],
            ..param()
        },
    )?;

    assert_eq!(plan.to_delete, set! {});
    Ok(())
}

#[test]
fn test_rejected_protected_feature_to_develop() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        local <<EOF
            git checkout -b feature
            touch awesome-patch
            git add awesome-patch
            git commit -m "Awesome patch"
            git push -u origin feature
        EOF

        # Create PR
        origin <<EOF
            git push upstream feature:refs/pull/1/head
        EOF

        # clicked delete branch button
        origin <<EOF
            git branch -D feature
        EOF
        "#,
    )?;

    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let plan = get_trim_plan(
        &git,
        &PlanParam {
            protected_patterns: vec!["refs/heads/feature"],
            ..param()
        },
    )?;

    assert_eq!(plan.to_delete, set! {});
    Ok(())
}
