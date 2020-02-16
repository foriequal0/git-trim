use std::ops::Deref;

use anyhow::Result;
use git2::{Config, ErrorClass, ErrorCode};
use std::str::FromStr;

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
    default: Option<&'a T>,
}

pub fn get_config<'a, T>(config: &'a Config, key: &'a str) -> ConfigBuilder<'a, T> {
    ConfigBuilder {
        config,
        key,
        explicit: None,
        default: None,
    }
}

impl<'a, T> ConfigBuilder<'a, T>
where
    T: Clone,
{
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

    pub fn with_default(self, value: &'a T) -> ConfigBuilder<'a, T> {
        ConfigBuilder {
            default: Some(value),
            ..self
        }
    }
}

impl<'a, T> ConfigBuilder<'a, T>
where
    T: ConfigValues + Clone,
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
                    Ok(Some(ConfigValue::Implicit(default.clone())))
                } else {
                    Ok(None)
                }
            }
            Err(err) => Err(err),
        }
    }
}

impl<'a, T> ConfigBuilder<'a, T>
where
    T: Clone,
{
    pub fn parse_with<F>(self, parse: F) -> Result<Option<ConfigValue<T>>>
    where
        F: FnOnce(&str) -> Result<T>,
    {
        if let Some((source, value)) = self.explicit {
            return Ok(Some(ConfigValue::Explicit {
                value,
                source: source.to_string(),
            }));
        }

        let result = match self.config.get_str(self.key) {
            Ok(value) => Some(ConfigValue::Explicit {
                value: parse(value)?,
                source: self.key.to_string(),
            }),
            Err(err) if config_not_exist(&err) => {
                if let Some(default) = self.default {
                    Some(ConfigValue::Implicit(default.clone()))
                } else {
                    None
                }
            }
            Err(err) => return Err(err.into()),
        };
        Ok(result)
    }
}

impl<'a, T> ConfigBuilder<'a, T>
where
    T: FromStr + Clone,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    pub fn parse(self) -> Result<Option<ConfigValue<T>>> {
        self.parse_with(|str| Ok(str.parse()?))
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

impl ConfigValues for bool {
    fn get_config_value(config: &Config, key: &str) -> Result<Self, git2::Error> {
        config.get_bool(key)
    }
}

fn config_not_exist(err: &git2::Error) -> bool {
    err.code() == ErrorCode::NotFound && err.class() == ErrorClass::Config
}
