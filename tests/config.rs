mod fixture;

use std::convert::TryFrom;
use std::iter::FromIterator;

use anyhow::Result;
use git2::Repository;

use git_trim::args::{Args, DeleteFilter, FilterUnit, Scope};
use git_trim::config::{CommaSeparatedSet, Config, ConfigValue};
use git_trim::Git;

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
        "#,
    )
}

#[test]
fn test_bases_implicit_value() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        local <<EOF
        EOF
        "#,
    )?;

    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let config = Config::read(&git.repo, &git.config, &Args::default())?;

    assert_eq!(
        config.bases,
        ConfigValue::Implicit(CommaSeparatedSet::new(vec!["master".to_owned()]))
    );
    Ok(())
}

#[test]
fn test_bases_config_value() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        local <<EOF
            git config trim.bases some-branch
        EOF
        "#,
    )?;

    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let config = Config::read(&git.repo, &git.config, &Args::default())?;

    assert_eq!(
        config.bases,
        ConfigValue::Explicit {
            value: CommaSeparatedSet::new(vec!["some-branch".to_owned(),]),
            source: "trim.bases".to_string(),
        }
    );
    Ok(())
}

#[test]
fn test_bases_args_value() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        local <<EOF
            git config trim.bases some-branch
        EOF
        "#,
    )?;

    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let config = Config::read(
        &git.repo,
        &git.config,
        &Args {
            bases: vec!["another-branch".to_owned()],
            ..Args::default()
        },
    )?;

    assert_eq!(
        config.bases,
        ConfigValue::Explicit {
            value: CommaSeparatedSet::new(vec!["another-branch".to_owned(),]),
            source: "cli".to_string(),
        }
    );
    Ok(())
}

// TODO: do we need to check explicit/implicit for other entries?

#[test]
fn test_bases_multiple_comma_separated_values() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        local <<EOF
            git config --add trim.bases a,b
            git config --add trim.bases c,d
        EOF
        "#,
    )?;

    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let config = Config::read(&git.repo, &git.config, &Args::default())?;

    assert_eq!(
        config.bases,
        ConfigValue::Explicit {
            value: CommaSeparatedSet::new(vec![
                "a".to_owned(),
                "b".to_owned(),
                "c".to_owned(),
                "d".to_owned(),
            ]),
            source: "trim.bases".to_string(),
        }
    );
    Ok(())
}

#[test]
fn test_protected_multiple_comma_separated_values() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        local <<EOF
            git config --add trim.protected a,b
            git config --add trim.protected c,d
        EOF
        "#,
    )?;

    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let config = Config::read(&git.repo, &git.config, &Args::default())?;

    assert_eq!(
        config.protected,
        ConfigValue::Explicit {
            value: CommaSeparatedSet::new(vec![
                "a".to_owned(),
                "b".to_owned(),
                "c".to_owned(),
                "d".to_owned(),
            ]),
            source: "trim.protected".to_string(),
        }
    );
    Ok(())
}

#[test]
fn test_delete_filter_multiple_comma_separated_values() -> Result<()> {
    let guard = fixture().prepare(
        "local",
        r#"
        local <<EOF
            git config --add trim.delete merged:origin,merged:upstream
            git config --add trim.delete stray,diverged:upstream
        EOF
        "#,
    )?;

    let git = Git::try_from(Repository::open(guard.working_directory())?)?;
    let config = Config::read(&git.repo, &git.config, &Args::default())?;

    assert_eq!(
        config.filter,
        ConfigValue::Explicit {
            value: DeleteFilter::from_iter(vec![
                FilterUnit::MergedLocal,
                FilterUnit::Stray,
                FilterUnit::MergedRemote(Scope::Scoped("origin".to_owned())),
                FilterUnit::MergedRemote(Scope::Scoped("upstream".to_owned())),
                FilterUnit::Diverged(Scope::Scoped("upstream".to_owned())),
            ]),
            source: "trim.delete".to_string(),
        }
    );
    Ok(())
}
