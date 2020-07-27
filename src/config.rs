use std::fmt::Debug;
use std::iter::FromIterator;
use std::ops::Deref;
use std::str::FromStr;

use anyhow::{anyhow, Result};
use git2::{Config, ErrorClass, ErrorCode};
use log::*;

type GitResult<T> = std::result::Result<T, git2::Error>;

#[derive(Debug)]
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
    config: &'a Config,
    key: &'a str,
    explicit: Option<(&'a str, T)>,
    default: Option<T>,
}

pub fn get<'a, T>(config: &'a Config, key: &'a str) -> ConfigBuilder<'a, T> {
    ConfigBuilder {
        config,
        key,
        explicit: None,
        default: None,
    }
}

impl<'a, T> ConfigBuilder<'a, T> {
    pub fn with_explicit(self, source: &'a str, value: Option<T>) -> ConfigBuilder<'a, T> {
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
    pub fn parses_and_collect<U>(self) -> Result<ConfigValue<U>>
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
    fn get_config_value(config: &Config, key: &str) -> Result<Self, git2::Error>
    where
        Self: Sized;
}

impl ConfigValues for String {
    fn get_config_value(config: &Config, key: &str) -> Result<Self, git2::Error> {
        config.get_string(key)
    }
}

impl ConfigValues for Vec<String> {
    fn get_config_value(config: &Config, key: &str) -> Result<Self, git2::Error> {
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
    fn get_config_value(config: &Config, key: &str) -> Result<Self, git2::Error> {
        config.get_bool(key)
    }
}

impl ConfigValues for u64 {
    fn get_config_value(config: &Config, key: &str) -> Result<Self, git2::Error> {
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

pub fn get_push_remote(config: &Config, refname: &str) -> Result<ConfigValue<String>> {
    let branch_name = short_local_branch_name(refname)?;

    let push_remote_key = format!("branch.{}.pushRemote", branch_name);
    if let Some(push_remote) = get(config, &push_remote_key).read()? {
        return Ok(push_remote);
    }

    if let Some(push_default) = get(config, "remote.pushDefault").read()? {
        return Ok(push_default);
    }

    get_remote(config, refname)
}

pub fn get_remote(config: &Config, refname: &str) -> Result<ConfigValue<String>> {
    let branch_name = short_local_branch_name(refname)?;

    Ok(get(config, &format!("branch.{}.remote", branch_name))
        .with_default(String::from("origin"))
        .read()?
        .expect("has default"))
}

pub fn get_remote_raw(config: &Config, refname: &str) -> Result<Option<String>> {
    let branch_name = short_local_branch_name(refname)?;

    let key = format!("branch.{}.remote", branch_name);
    match config.get_string(&key) {
        Ok(remote) => Ok(Some(remote)),
        Err(err) if config_not_exist(&err) => Ok(None),
        Err(err) => Err(err.into()),
    }
}

pub fn get_merge(config: &Config, refname: &str) -> Result<Option<String>> {
    let branch_name = short_local_branch_name(refname)?;

    let key = format!("branch.{}.merge", branch_name);
    match config.get_string(&key) {
        Ok(merge) => Ok(Some(merge)),
        Err(err) if config_not_exist(&err) => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn short_local_branch_name(refname: &str) -> Result<&str> {
    if refname.starts_with("refs/heads/") {
        Ok(&refname["refs/heads/".len()..])
    } else if !refname.starts_with("refs/") {
        Ok(refname)
    } else {
        Err(anyhow!("It is not a local branch"))
    }
}

#[derive(Debug, Clone, Default)]
pub struct CommaSeparatedSet<T>(Vec<T>);

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
