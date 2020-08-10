use std::convert::TryFrom;
use std::fmt::Debug;
use std::iter::FromIterator;
use std::ops::Deref;
use std::str::FromStr;

use anyhow::{Context, Result};
use git2::{BranchType, Config as GitConfig, Error, ErrorClass, ErrorCode, Remote, Repository};
use log::*;

use crate::args::{Args, DeleteFilter};
use crate::branch::LocalBranch;

type GitResult<T> = std::result::Result<T, git2::Error>;

#[derive(Debug)]
pub struct Config {
    pub bases: ConfigValue<CommaSeparatedSet<String>>,
    pub protected: ConfigValue<CommaSeparatedSet<String>>,
    pub update: ConfigValue<bool>,
    pub update_interval: ConfigValue<u64>,
    pub confirm: ConfigValue<bool>,
    pub detach: ConfigValue<bool>,
    pub filter: ConfigValue<DeleteFilter>,
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

        let bases = get(config, "trim.bases")
            .with_explicit("cli", non_empty(args.bases.clone()))
            .with_default(get_branches_tracks_remote_heads(repo, config)?)
            .parses_and_collect::<CommaSeparatedSet<String>>()?;
        let protected = get(config, "trim.protected")
            .with_explicit("cli", non_empty(args.protected.clone()))
            .parses_and_collect::<CommaSeparatedSet<String>>()?;
        let update = get(config, "trim.update")
            .with_explicit("cli", args.update())
            .with_default(true)
            .read()?
            .expect("has default");
        let update_interval = get(config, "trim.updateInterval")
            .with_explicit("cli", args.update_interval)
            .with_default(5)
            .read()?
            .expect("has default");
        let confirm = get(config, "trim.confirm")
            .with_explicit("cli", args.confirm())
            .with_default(true)
            .read()?
            .expect("has default");
        let detach = get(config, "trim.detach")
            .with_explicit("cli", args.detach())
            .with_default(true)
            .read()?
            .expect("has default");
        let filter = get(config, "trim.delete")
            .with_explicit("cli", non_empty(args.delete.clone()))
            .with_default(vec![DeleteFilter::merged_origin()])
            .parses_and_collect::<DeleteFilter>()?;

        Ok(Config {
            bases,
            protected,
            update,
            update_interval,
            confirm,
            detach,
            filter,
        })
    }
}

fn get_branches_tracks_remote_heads(repo: &Repository, config: &GitConfig) -> Result<Vec<String>> {
    let mut result = Vec::new();
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

        for branch in repo.branches(Some(BranchType::Local))? {
            let (branch, _) = branch?;
            let branch = LocalBranch::try_from(&branch)?;

            if let Some(upstream) = branch.fetch_upstream(repo, config)? {
                if upstream.refname == refname {
                    result.push(branch.short_name().to_owned());
                }
            }
        }
    }
    Ok(result)
}

#[derive(Debug, Eq, PartialEq)]
pub enum ConfigValue<T> {
    Explicit { value: T, source: String },
    Implicit(T),
}

impl<T> ConfigValue<T> {
    pub fn unwrap(self) -> T {
        match self {
            ConfigValue::Explicit { value: x, .. } | ConfigValue::Implicit(x) => x,
        }
    }

    pub fn is_implicit(&self) -> bool {
        match self {
            ConfigValue::Explicit { .. } => false,
            ConfigValue::Implicit(_) => true,
        }
    }
}

impl<T> Deref for ConfigValue<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        match self {
            ConfigValue::Explicit { value: x, .. } | ConfigValue::Implicit(x) => x,
        }
    }
}

pub struct ConfigBuilder<'a, T> {
    config: &'a GitConfig,
    key: &'a str,
    explicit: Option<(&'a str, T)>,
    default: Option<T>,
}

pub fn get<'a, T>(config: &'a GitConfig, key: &'a str) -> ConfigBuilder<'a, T> {
    ConfigBuilder {
        config,
        key,
        explicit: None,
        default: None,
    }
}

impl<'a, T> ConfigBuilder<'a, T> {
    fn with_explicit(self, source: &'a str, value: Option<T>) -> ConfigBuilder<'a, T> {
        if let Some(value) = value {
            ConfigBuilder {
                explicit: Some((source, value)),
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
        if let Some((source, value)) = self.explicit {
            return Ok(Some(ConfigValue::Explicit {
                value,
                source: source.to_string(),
            }));
        }
        match T::get_config_value(self.config, self.key) {
            Ok(value) => Ok(Some(ConfigValue::Explicit {
                value,
                source: self.key.to_string(),
            })),
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
        U: FromStr + FromIterator<<T as IntoIterator>::Item> + FromIterator<U> + Default,
        U::Err: std::error::Error + Send + Sync + 'static,
    {
        if let Some((source, value)) = self.explicit {
            return Ok(ConfigValue::Explicit {
                value: value.into_iter().collect(),
                source: source.to_string(),
            });
        }

        let result = match Vec::<String>::get_config_value(self.config, self.key) {
            Ok(values) if !values.is_empty() => {
                let mut result = Vec::new();
                for x in values {
                    result.push(U::from_str(&x)?)
                }

                ConfigValue::Explicit {
                    value: result.into_iter().collect(),
                    source: self.key.to_string(),
                }
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
        for entry in &config.entries(Some(key))? {
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

    get_remote_name(config, branch)
}

pub fn get_remote_name(config: &GitConfig, branch: &LocalBranch) -> Result<String> {
    Ok(get_remote_name_raw(config, branch)?.unwrap_or_else(|| "origin".to_owned()))
}

pub fn get_remote_name_raw(config: &GitConfig, branch: &LocalBranch) -> Result<Option<String>> {
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

#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub struct CommaSeparatedSet<T>(Vec<T>);

impl<T> CommaSeparatedSet<T>
where
    T: PartialEq,
{
    pub fn new(value: Vec<T>) -> Self {
        Self::from_iter(value)
    }
}

impl<T> FromStr for CommaSeparatedSet<T>
where
    T: FromStr + PartialEq,
{
    type Err = T::Err;

    fn from_str(args: &str) -> Result<Self, Self::Err> {
        let mut result = Vec::new();
        for arg in args.split(',') {
            let parsed: T = arg.trim().parse()?;
            result.push(parsed);
        }
        Ok(Self::from_iter(result))
    }
}

impl<T> FromIterator<T> for CommaSeparatedSet<T>
where
    T: PartialEq,
{
    fn from_iter<I>(iter: I) -> Self
    where
        I: IntoIterator<Item = T>,
    {
        let mut result = Vec::new();
        for item in iter.into_iter() {
            if !result.contains(&item) {
                result.push(item);
            }
        }
        Self(result)
    }
}

impl<T> FromIterator<Self> for CommaSeparatedSet<T>
where
    T: PartialEq,
{
    fn from_iter<I>(iter: I) -> Self
    where
        I: IntoIterator<Item = Self>,
    {
        iter.into_iter().flatten().collect()
    }
}

impl<T> IntoIterator for CommaSeparatedSet<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<T> Deref for CommaSeparatedSet<T> {
    type Target = Vec<T>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
