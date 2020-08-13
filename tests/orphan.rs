mod fixture;

use std::convert::TryFrom;

use anyhow::Result;
use git2::Repository;

use git_trim::args::DeleteFilter;

use git_trim::{get_trim_plan, Git, PlanParam};

use fixture::{rc, Fixture};

fn fixture() -> Fixture {
    rc().append_fixture_trace(
        r#"
        git init origin
        origin <<EOF
            git config user.name "UpstreamTest"
            git config user.email "upstream@test"
            echo "Hello World!" > README.md
            git add README.md
            git commit -m "Initial commit"
        EOF

        git clone origin local
        local <<EOF
            git config user.name "LocalTest"
            git config user.email "local@test"
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
fn test_bases_implicit_value() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        local <<EOF
            git checkout --orphan new-test
            touch some-file
            git add some-file
            git commit -m "just testing"
            git push -u origin new-test
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

    assert_eq!(plan.to_delete, set! {});

    Ok(())
}
