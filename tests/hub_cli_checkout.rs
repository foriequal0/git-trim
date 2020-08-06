mod fixture;

use std::convert::TryFrom;

use anyhow::Result;
use git2::Repository;

use git_trim::args::DeleteFilter;
use git_trim::{get_trim_plan, ClassifiedBranch, Git, LocalBranch, PlanParam, RemoteBranch};

use fixture::{rc, Fixture};

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
        EOF
        git clone upstream origin -o upstream
        origin <<EOF
            git config user.name "Origin Test"
            git config user.email "origin@test"
            git config remote.pushdefault upstream
        EOF
        git clone origin local
        local <<EOF
            git config user.name "Local Test"
            git config user.email "local@test"
            git config remote.pushdefault origin
            git config push.default simple
            git remote add upstream ../upstream
            git fetch upstream
            git branch -u upstream/master master
        EOF
        origin <<EOF
            git checkout -b feature
            touch awesome-patch
            git add awesome-patch
            git commit -m "Awesome patch"
            git push upstream feature:refs/pull/1/head
        EOF
        "#,
    )
}

fn param() -> PlanParam<'static> {
    PlanParam {
        bases: vec!["master"],
        protected_branches: set! {},
        filter: DeleteFilter::all(),
        detach: true,
    }
}

#[test]
fn test_noop() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        local <<EOF
            git fetch ../origin feature:feature
            git config branch.feature.remote "../origin"
            git config branch.feature.merge "refs/heads/feature"
        EOF
        "#,
    )?;

    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let plan = get_trim_plan(&git, &param())?;
    assert_eq!(plan.to_delete, set! {});
    Ok(())
}

#[test]
fn test_accepted() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        local <<EOF
            git fetch ../origin feature:feature
            git config branch.feature.remote "../origin"
            git config branch.feature.merge "refs/heads/feature"
        EOF
        upstream <<EOF
            git merge refs/pull/1/head
        EOF
        # clicked delete branch button
        origin <<EOF
            git checkout master
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
fn test_accepted_but_forgot_to_delete() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        local <<EOF
            git fetch ../origin feature:feature
            git config branch.feature.remote "../origin"
            git config branch.feature.merge "refs/heads/feature"
        EOF
        upstream <<EOF
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
            ClassifiedBranch::MergedRemote(
                RemoteBranch {
                    remote: "../origin".to_string(),
                    refname: "refs/heads/feature".to_string(),
                },
            ),
        },
    );
    Ok(())
}

#[test]
fn test_modified_and_accepted() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        local <<EOF
            git fetch ../origin feature:feature
            git config branch.feature.remote "../origin"
            git config branch.feature.merge "refs/heads/feature"
        EOF
        origin <<EOF
            touch another-patch
            git add another-patch
            git commit -m "another patch"
            git push upstream feature:refs/pull/1/head
        EOF
        upstream <<EOF
            git merge refs/pull/1/head
        EOF
        # click delete button
        origin <<EOF
            git checkout master
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
fn test_modified_and_accepted_but_forgot_to_delete() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        local <<EOF
            git fetch ../origin feature:feature
            git config branch.feature.remote "../origin"
            git config branch.feature.merge "refs/heads/feature"
        EOF
        origin <<EOF
            touch another-patch
            git add another-patch
            git commit -m "another patch"
            git push upstream feature:refs/pull/1/head
        EOF
        upstream <<EOF
            git merge refs/pull/1/head
        EOF
        "#,
    )?;

    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let plan = get_trim_plan(&git, &param())?;
    assert_eq!(
        plan.to_delete,
        set! {
            ClassifiedBranch::Diverged {
                local: LocalBranch::new("refs/heads/feature"),
                remote: RemoteBranch {
                    remote: "../origin".to_string(),
                    refname: "refs/heads/feature".to_string(),
                },
            },
        },
    );
    Ok(())
}

#[test]
fn test_rejected() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        local <<EOF
            git fetch ../origin feature:feature
            git config branch.feature.remote "../origin"
            git config branch.feature.merge "refs/heads/feature"
        EOF
        # clicked delete branch button
        origin <<EOF
            git checkout master
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
fn test_should_not_push_delete_non_heads() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        local <<EOF
            git fetch ../upstream refs/pull/1/head:feature
            git config branch.feature.remote "../origin"
            git config branch.feature.merge "refs/pull/1/head"
        EOF
        upstream <<EOF
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
        },
    );
    Ok(())
}
