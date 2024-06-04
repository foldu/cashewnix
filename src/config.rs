use std::{num::NonZeroU16, path::Path, time::Duration};

use ahash::HashMap;
use eyre::Context;
use ring_compat::signature::ed25519::VerifyingKey;
use serde::{de::Error as _, Deserialize, Deserializer};

use crate::{
    binary_cache_proxy::ErrorStrategy,
    discover::proto::LocalCache,
    signing::parse_ssh_public_key,
};

// NOTE: when adding or editing a field create a new test config
// with ./scripts/new_test_config.sh
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub priority: u8,
    pub port: NonZeroU16,
    #[serde(default = "default_cache_timeout", with = "humantime_serde")]
    pub default_cache_timeout: Duration,
    pub binary_caches: Vec<BinaryCache>,
    #[serde(deserialize_with = "deserialize_public_keys")]
    pub public_keys: Vec<VerifyingKey>,
    pub local_binary_caches: Option<BinaryCacheConfig>,
    pub priority_config: HashMap<u8, PriorityConfig>,
}

fn deserialize_public_keys<'de, D>(de: D) -> Result<Vec<VerifyingKey>, D::Error>
where
    D: Deserializer<'de>,
{
    let array: Vec<&str> = Deserialize::deserialize(de)?;
    array
        .into_iter()
        .map(parse_ssh_public_key)
        .collect::<Result<Vec<_>, _>>()
        .map_err(D::Error::custom)
}

#[derive(Debug, Clone, Deserialize)]
pub struct PriorityConfig {
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BinaryCache {
    pub url: url::Url,
    pub priority: u8,
    #[serde(default = "timeout_strategy_default")]
    pub error_strategy: ErrorStrategy,
}

fn timeout_strategy_default() -> ErrorStrategy {
    ErrorStrategy::Timeout {
        timeout: Duration::from_secs(5),
    }
}

fn remove_strategy_default() -> ErrorStrategy {
    ErrorStrategy::Remove
}

fn default_cache_timeout() -> Duration {
    Duration::from_secs(5)
}

#[derive(Debug, Clone, Deserialize)]
pub struct BinaryCacheConfig {
    #[serde(with = "humantime_serde")]
    pub discovery_refresh_time: Duration,
    pub local_cache: Option<LocalCache>,
    #[serde(default = "remove_strategy_default")]
    pub error_strategy: ErrorStrategy,
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, eyre::Error> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed reading file from {}", path.display()))?;
        let config: Config = serde_json::from_str(&content)
            .with_context(|| format!("Failed deserializing config from {}", path.display()))?;

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn example_config_deserializes() {
        for config in std::fs::read_dir("./data/configs").unwrap() {
            let config = config.unwrap();
            Config::load(config.path()).unwrap();
        }
    }
}
