mod fixture;

use std::convert::TryFrom;

use anyhow::Result;
use git2::Repository;

use git_trim::{get_trim_plan, ClassifiedBranch, Git, LocalBranch};

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

#[test]
fn test_bases_implicit_value() -> Result<()> {
    let guard = fixture().prepare("local", r"")?;

    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let plan = get_trim_plan(&git, &test_default_param())?;

    assert!(plan.preserved.iter().any(|w| {
        w.branch == ClassifiedBranch::MergedLocal(LocalBranch::new("refs/heads/worktree"))
            && w.reason.contains("worktree")
    }));
    Ok(())
}
