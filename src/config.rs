use std::convert::TryFrom;
use std::fmt::Debug;
use std::iter::FromIterator;
use std::ops::Deref;
use std::str::FromStr;

use anyhow::{Context, Result};
use git2::{BranchType, Config as GitConfig, Error, ErrorClass, ErrorCode, Remote, Repository};
use log::*;

use crate::args::{Args, DeleteFilter, DeleteRange};
use crate::branch::{LocalBranch, RemoteTrackingBranchStatus};
use std::collections::HashSet;

type GitResult<T> = std::result::Result<T, git2::Error>;

#[derive(Debug)]
pub struct Config {
    pub bases: ConfigValue<HashSet<String>>,
    pub protected: ConfigValue<Vec<String>>,
    pub update: ConfigValue<bool>,
    pub update_interval: ConfigValue<u64>,
    pub confirm: ConfigValue<bool>,
    pub detach: ConfigValue<bool>,
    pub delete: ConfigValue<DeleteFilter>,
}

impl Config {
    pub fn read(repo: &Repository, config: &GitConfig, args: &Args) -> Result<Self> {
        fn non_empty<T>(x: Vec<T>) -> Option<Vec<T>> {
            if x.is_empty() {
                None
            } else {
                Some(x)
            }
        }

        let bases = get_comma_separated_multi(config, "trim.bases")
            .with_explicit(non_empty(args.bases.clone()))
            .with_default(get_branches_tracks_remote_heads(repo, config)?)
            .parses_and_collect::<HashSet<String>>()?;
        let protected = get_comma_separated_multi(config, "trim.protected")
            .with_explicit(non_empty(args.protected.clone()))
            .parses_and_collect::<Vec<String>>()?;
        let update = get(config, "trim.update")
            .with_explicit(args.update())
            .with_default(true)
            .read()?
            .expect("has default");
        let update_interval = get(config, "trim.updateInterval")
            .with_explicit(args.update_interval)
            .with_default(5)
            .read()?
            .expect("has default");
        let confirm = get(config, "trim.confirm")
            .with_explicit(args.confirm())
            .with_default(true)
            .read()?
            .expect("has default");
        let detach = get(config, "trim.detach")
            .with_explicit(args.detach())
            .with_default(true)
            .read()?
            .expect("has default");
        let delete = get_comma_separated_multi(config, "trim.delete")
            .with_explicit(non_empty(args.delete.clone()))
            .with_default(DeleteRange::merged_origin())
            .parses_and_collect::<DeleteFilter>()?;

        Ok(Config {
            bases,
            protected,
            update,
            update_interval,
            confirm,
            detach,
            delete,
        })
    }
}

fn get_branches_tracks_remote_heads(repo: &Repository, config: &GitConfig) -> Result<Vec<String>> {
    let mut local_bases = Vec::new();
    let mut all_bases = Vec::new();

    for reference in repo.references_glob("refs/remotes/*/HEAD")? {
        let reference = reference?;
        // git symbolic-ref refs/remotes/*/HEAD
        let resolved = match reference.resolve() {
            Ok(resolved) => resolved,
            Err(_) => {
                debug!(
                    "Reference {:?} is expected to be an symbolic ref, but it isn't",
                    reference.name()
                );
                continue;
            }
        };
        let refname = resolved.name().context("non utf-8 reference name")?;
        all_bases.push(refname.to_owned());

        for branch in repo.branches(Some(BranchType::Local))? {
            let (branch, _) = branch?;
            let branch = LocalBranch::try_from(&branch)?;

            if let RemoteTrackingBranchStatus::Exists(upstream) =
                branch.fetch_upstream(repo, config)?
            {
                if upstream.refname == refname {
                    local_bases.push(branch.short_name().to_owned());
                }
            }
        }
    }

    if local_bases.is_empty() {
        Ok(all_bases)
    } else {
        Ok(local_bases)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum ConfigValue<T> {
    Explicit(T),
    GitConfig(T),
    Implicit(T),
}

impl<T> ConfigValue<T> {
    pub fn unwrap(self) -> T {
        match self {
            ConfigValue::Explicit(x) | ConfigValue::GitConfig(x) | ConfigValue::Implicit(x) => x,
        }
    }

    pub fn is_implicit(&self) -> bool {
        match self {
            ConfigValue::Explicit(_) => false,
            ConfigValue::GitConfig(_) => false,
            ConfigValue::Implicit(_) => true,
        }
    }
}

impl<T> Deref for ConfigValue<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        match self {
            ConfigValue::Explicit(x) | ConfigValue::GitConfig(x) | ConfigValue::Implicit(x) => x,
        }
    }
}

pub struct ConfigBuilder<'a, T> {
    config: &'a GitConfig,
    key: &'a str,
    explicit: Option<T>,
    default: Option<T>,
    comma_separated: bool,
}

pub fn get<'a, T>(config: &'a GitConfig, key: &'a str) -> ConfigBuilder<'a, T> {
    ConfigBuilder {
        config,
        key,
        explicit: None,
        default: None,
        comma_separated: false,
    }
}

pub fn get_comma_separated_multi<'a, T>(
    config: &'a GitConfig,
    key: &'a str,
) -> ConfigBuilder<'a, T> {
    ConfigBuilder {
        config,
        key,
        explicit: None,
        default: None,
        comma_separated: true,
    }
}

impl<'a, T> ConfigBuilder<'a, T> {
    fn with_explicit(self, value: Option<T>) -> ConfigBuilder<'a, T> {
        if let Some(value) = value {
            ConfigBuilder {
                explicit: Some(value),
                ..self
            }
        } else {
            self
        }
    }

    pub fn with_default(self, value: T) -> ConfigBuilder<'a, T> {
        ConfigBuilder {
            default: Some(value),
            ..self
        }
    }
}

impl<'a, T> ConfigBuilder<'a, T>
where
    T: ConfigValues,
{
    pub fn read(self) -> GitResult<Option<ConfigValue<T>>> {
        if let Some(value) = self.explicit {
            return Ok(Some(ConfigValue::Explicit(value)));
        }
        match T::get_config_value(self.config, self.key) {
            Ok(value) => Ok(Some(ConfigValue::GitConfig(value))),
            Err(err) if config_not_exist(&err) => {
                if let Some(default) = self.default {
                    Ok(Some(ConfigValue::Implicit(default)))
                } else {
                    Ok(None)
                }
            }
            Err(err) => Err(err),
        }
    }
}

impl<'a, T> ConfigBuilder<'a, T> {
    fn parses_and_collect<U>(self) -> Result<ConfigValue<U>>
    where
        T: IntoIterator,
        T::Item: FromStr,
        <T::Item as FromStr>::Err: std::error::Error + Send + Sync + 'static,
        U: FromIterator<<T as IntoIterator>::Item> + Default,
    {
        if let Some(value) = self.explicit {
            return Ok(ConfigValue::Explicit(value.into_iter().collect()));
        }

        let result = match Vec::<String>::get_config_value(self.config, self.key) {
            Ok(entries) if !entries.is_empty() => {
                let mut result = Vec::new();
                if self.comma_separated {
                    for entry in entries {
                        for item in entry.split(',') {
                            if !item.is_empty() {
                                let value = <T::Item>::from_str(item)?;
                                result.push(value);
                            }
                        }
                    }
                } else {
                    for entry in entries {
                        let value = <T::Item>::from_str(&entry)?;
                        result.push(value);
                    }
                }

                ConfigValue::GitConfig(result.into_iter().collect())
            }
            Ok(_) => {
                if let Some(default) = self.default {
                    ConfigValue::Implicit(default.into_iter().collect())
                } else {
                    ConfigValue::Implicit(U::default())
                }
            }
            Err(err) => return Err(err.into()),
        };
        Ok(result)
    }
}

pub trait ConfigValues {
    fn get_config_value(config: &GitConfig, key: &str) -> Result<Self, git2::Error>
    where
        Self: Sized;
}

impl ConfigValues for String {
    fn get_config_value(config: &GitConfig, key: &str) -> Result<Self, git2::Error> {
        config.get_string(key)
    }
}

impl ConfigValues for Vec<String> {
    fn get_config_value(config: &GitConfig, key: &str) -> Result<Self, git2::Error> {
        let mut result = Vec::new();
        let mut entries = config.entries(Some(key))?;
        while let Some(entry) = entries.next() {
            let entry = entry?;
            if let Some(value) = entry.value() {
                result.push(value.to_owned());
            } else {
                warn!(
                    "non utf-8 config entry {}",
                    String::from_utf8_lossy(entry.name_bytes())
                );
            }
        }
        Ok(result)
    }
}

impl ConfigValues for bool {
    fn get_config_value(config: &GitConfig, key: &str) -> Result<Self, git2::Error> {
        config.get_bool(key)
    }
}

impl ConfigValues for u64 {
    fn get_config_value(config: &GitConfig, key: &str) -> Result<Self, git2::Error> {
        let value = config.get_i64(key)?;
        if value >= 0 {
            return Ok(value as u64);
        }
        panic!("`git config {}` cannot be negative value", key);
    }
}

fn config_not_exist(err: &git2::Error) -> bool {
    err.code() == ErrorCode::NotFound && err.class() == ErrorClass::Config
}

pub fn get_push_remote(config: &GitConfig, branch: &LocalBranch) -> Result<String> {
    let push_remote_key = format!("branch.{}.pushRemote", branch.short_name());
    if let Some(push_remote) = get::<String>(config, &push_remote_key).read()? {
        return Ok(push_remote.unwrap());
    }

    if let Some(push_default) = get::<String>(config, "remote.pushDefault").read()? {
        return Ok(push_default.unwrap());
    }

    Ok(get_remote_name(config, branch)?.unwrap_or_else(|| "origin".to_owned()))
}

pub fn get_remote_name(config: &GitConfig, branch: &LocalBranch) -> Result<Option<String>> {
    let key = format!("branch.{}.remote", branch.short_name());
    match config.get_string(&key) {
        Ok(remote) => Ok(Some(remote)),
        Err(err) if config_not_exist(&err) => Ok(None),
        Err(err) => Err(err.into()),
    }
}

pub fn get_remote<'a>(repo: &'a Repository, remote_name: &str) -> Result<Option<Remote<'a>>> {
    fn error_is_missing_remote(err: &Error) -> bool {
        err.class() == ErrorClass::Config && err.code() == ErrorCode::InvalidSpec
    }

    match repo.find_remote(remote_name) {
        Ok(remote) => Ok(Some(remote)),
        Err(err) if error_is_missing_remote(&err) => Ok(None),
        Err(err) => Err(err.into()),
    }
}

pub fn get_merge(config: &GitConfig, branch: &LocalBranch) -> Result<Option<String>> {
    let key = format!("branch.{}.merge", branch.short_name());
    match config.get_string(&key) {
        Ok(merge) => Ok(Some(merge)),
        Err(err) if config_not_exist(&err) => Ok(None),
        Err(err) => Err(err.into()),
    }
}
