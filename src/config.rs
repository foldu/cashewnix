use std::{num::NonZeroU16, path::Path, time::Duration};

use ahash::HashMap;
use eyre::Context;
use ring_compat::signature::ed25519::VerifyingKey;
use serde::{de::Error as _, Deserialize, Deserializer};

use crate::{discover::proto::LocalCache, signing::parse_ssh_public_key};

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub port: NonZeroU16,
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
}

#[derive(Debug, Clone, Deserialize)]
pub struct BinaryCacheConfig {
    #[serde(with = "humantime_serde")]
    pub discovery_refresh_time: Duration,
    pub local_cache: Option<LocalCache>,
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
        Config::load("data/config.json").unwrap();
    }
}
