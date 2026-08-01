use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::bail;
use serde::Deserialize;

/// Settings shared by every account in the pool.
#[derive(Debug, Deserialize)]
pub struct PoolSettings {
    #[serde(default = "default_listen_addr")]
    pub listen_addr: String,
    #[serde(default = "default_upstream_base")]
    pub upstream_base: String,
    pub default_switch_at_percent: f64,
    #[serde(rename = "profile")]
    pub profiles: Vec<ProfileSettings>,
}

fn default_listen_addr() -> String {
    "127.0.0.1:8788".to_string()
}

fn default_upstream_base() -> String {
    "https://chatgpt.com/backend-api/codex".to_string()
}

/// Settings for one account.
#[derive(Debug, Deserialize)]
pub struct ProfileSettings {
    pub label: String,
    pub home: PathBuf,
    pub priority: u32,
    pub switch_at_percent: Option<f64>,
    #[serde(default)]
    pub is_main: bool,
}

impl PoolSettings {
    /// Loads a settings file and reports the line containing invalid TOML.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let input = fs::read_to_string(path)
            .with_context(|| format!("could not read settings file {}", path.display()))?;
        toml::from_str(&input).map_err(|source| {
            let line = source
                .span()
                .map(|span| {
                    input.as_bytes()[..span.start]
                        .iter()
                        .filter(|byte| **byte == b'\n')
                        .count()
                        + 1
                })
                .unwrap_or(1);
            anyhow!(
                "settings file {} is invalid at line {line}: {source}",
                path.display()
            )
        })
    }

    /// Uses the account override when present, otherwise the global percentage.
    pub fn switch_at_percent_for(&self, profile: &ProfileSettings) -> f64 {
        profile
            .switch_at_percent
            .unwrap_or(self.default_switch_at_percent)
    }

    /// Rejects settings that could select accounts unsafely.
    pub fn validate(&self) -> Result<()> {
        let mut priorities = HashMap::new();
        for profile in &self.profiles {
            if let Some(previous_label) = priorities.insert(profile.priority, &profile.label) {
                bail!(
                    "priority {} is shared by accounts '{}' and '{}'; give each account a unique priority number",
                    profile.priority,
                    previous_label,
                    profile.label
                );
            }
        }

        if let Some(main) = self.profiles.iter().find(|profile| profile.is_main)
            && self
                .profiles
                .iter()
                .any(|profile| profile.priority > main.priority)
        {
            bail!(
                "main account '{}' must have the highest priority number so it is used last; increase its priority",
                main.label
            );
        }

        for profile in &self.profiles {
            if !profile.home.exists() {
                bail!(
                    "account '{}' folder '{}' does not exist; create it or fix home in the settings file",
                    profile.label,
                    profile.home.display()
                );
            }
        }

        Ok(())
    }
}
