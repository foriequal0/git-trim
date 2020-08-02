mod fixture;

use std::convert::TryFrom;

use anyhow::Result;
use git2::Repository;

use git_trim::args::DeleteFilter;

use git_trim::{get_trim_plan, ClassifiedBranch, Git, LocalBranch, PlanParam};

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
            git worktree add ../worktree
        EOF

        within worktree <<EOF
            git config user.name "WorktreeTest"
            git config user.email "worktree@test"

            echo "Yay" >> README.md
            git add README.md
            git commit -m "Yay"
            git push -u origin worktree
        EOF

        origin <<EOF
            git merge worktree
            git branch -d worktree
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
    let guard = fixture().prepare("local", r#""#)?;

    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let plan = get_trim_plan(
        &git,
        &PlanParam {
            filter: DeleteFilter::all(),
            ..param()
        },
    )?;

    assert!(plan.preserved.iter().any(|w| {
        w.branch == ClassifiedBranch::MergedLocal(LocalBranch::new("refs/heads/worktree"))
            && w.reason.contains("worktree")
    }));
    Ok(())
}
