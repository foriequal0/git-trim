use std::fmt::Write;
use std::io::{BufRead, BufReader, Error, Write as _};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread::spawn;

use log::*;
use tempfile::{tempdir, TempDir};

#[derive(Default)]
pub struct Fixture {
    fixture: String,
    epilogue: String,
}

impl Fixture {
    pub fn new() -> Fixture {
        Fixture::default()
    }

    pub fn append_silent_fixture(&self, appended: &str) -> Fixture {
        let mut fixture = String::new();
        writeln!(fixture, "{}", self.fixture).unwrap();
        writeln!(fixture, "{{").unwrap();
        writeln!(fixture, "{}", textwrap::dedent(appended)).unwrap();
        writeln!(fixture, "}} &> /dev/null").unwrap();
        Fixture {
            fixture,
            epilogue: self.epilogue.clone(),
        }
    }

    pub fn append_fixture(&self, appended: &str) -> Fixture {
        let mut fixture = String::new();
        writeln!(fixture, "{}", self.fixture).unwrap();
        writeln!(fixture, "{}", textwrap::dedent(appended)).unwrap();
        Fixture {
            fixture,
            epilogue: self.epilogue.clone(),
        }
    }

    fn append_epilogue(&self, appended: &str) -> Fixture {
        let mut epilogue = String::new();
        writeln!(epilogue, "{}", self.epilogue).unwrap();
        writeln!(epilogue, "{}", textwrap::dedent(appended)).unwrap();
        Fixture {
            fixture: self.fixture.clone(),
            epilogue,
        }
    }

    pub fn prepare(
        &self,
        working_directory: &str,
        last_fixture: &str,
    ) -> std::io::Result<FixtureGuard> {
        let _ = env_logger::builder().is_test(true).try_init();

        let tempdir = tempdir()?;
        println!("{:?}", tempdir.path());
        let mut bash = Command::new("bash")
            .args(&["--noprofile", "--norc", "-eo", "pipefail"])
            .current_dir(tempdir.path())
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let mut stdin = bash.stdin.take().unwrap();
        writeln!(stdin, "{}", self.fixture).unwrap();
        writeln!(stdin, "{}", textwrap::dedent(last_fixture)).unwrap();
        writeln!(stdin, "{}", self.epilogue).unwrap();
        drop(stdin);

        let stdout_thread = spawn({
            let stdout = bash.stdout.take().unwrap();
            move || {
                for line in BufReader::new(stdout).lines() {
                    match line {
                        Ok(line) if line.starts_with("+") => trace!("{}", line),
                        Ok(line) => info!("{}", line),
                        Err(err) => error!("{}", err),
                    }
                }
            }
        });

        let stderr_thread = spawn({
            let stderr = bash.stderr.take().unwrap();
            move || {
                for line in BufReader::new(stderr).lines() {
                    match line {
                        Ok(line) if line.starts_with("+") => trace!("{}", line),
                        Ok(line) => info!("{}", line),
                        Err(err) => error!("{}", err),
                    }
                }
            }
        });
        stdout_thread.join().unwrap();
        stderr_thread.join().unwrap();

        let exit_status = bash.wait()?;
        if !exit_status.success() {
            return Err(Error::from_raw_os_error(exit_status.code().unwrap_or(-1)));
        }

        let previous_pwd = std::env::current_dir()?;
        std::env::set_current_dir(tempdir.path().join(working_directory))?;
        Ok(FixtureGuard {
            _tempdir: tempdir,
            previous_pwd,
        })
    }
}

#[must_use]
pub struct FixtureGuard {
    _tempdir: TempDir,
    previous_pwd: PathBuf,
}

impl<'a> Drop for FixtureGuard {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.previous_pwd).unwrap();
    }
}

pub fn rc() -> Fixture {
    Fixture::new()
        .append_silent_fixture(
            r#"
            ## rc begin
            shopt -s expand_aliases
            within() {
                pushd $1 > /dev/null
                source /dev/stdin
                popd > /dev/null
            }
            alias upstream='within upstream'
            alias origin='within origin'
            alias local='within local'
            ## rc ends
            "#,
        )
        .append_epilogue(
            r#"
            ## epilogue begins
            local <<EOF
                pwd
                git remote update --prune
                git branch -vv
                git log --oneline --oneline --decorate --graph
            EOF
            ## epilogue ends
            "#,
        )
}

#[macro_export]
macro_rules! set {
    {$($x:expr),*} => ({
        use ::std::collections::HashSet;
        use ::std::convert::From;

        let mut tmp = HashSet::new();
        $(tmp.insert(From::from($x));)*
        tmp
    });
    {$($x:expr,)*} => ($crate::set!{$($x),*})
}

#[test]
#[ignore]
fn test() -> std::io::Result<()> {
    let _guard = Fixture::new()
        .append_epilogue("echo 'epilogue'")
        .append_fixture("echo 'fixture'")
        .prepare("", "");
    Ok(())
}
