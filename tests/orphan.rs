mod fixture;

use std::convert::TryFrom;
use std::iter::FromIterator;

use anyhow::Result;
use git2::Repository;

use git_trim::args::{DeleteFilter, DeleteRange, Scope};

use git_trim::{get_trim_plan, Git, PlanParam};

use fixture::{rc, test_default_param, Fixture};

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

#[test]
fn test_orphan_should_not_crash() -> Result<()> {
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
            delete: DeleteFilter::from_iter(vec![
                DeleteRange::MergedLocal,
                DeleteRange::MergedRemote(Scope::All),
                DeleteRange::Stray,
                DeleteRange::Diverged(Scope::All),
                DeleteRange::Local,
                DeleteRange::Remote(Scope::All),
            ]),
            ..test_default_param()
        },
    )?;

    assert_eq!(plan.to_delete, set! {});

    Ok(())
}
