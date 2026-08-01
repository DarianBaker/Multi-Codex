use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;
use serde::Serialize;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub(super) struct AccountUsage {
    pub(super) used_percent: f64,
    pub(super) window_minutes: i64,
    pub(super) resets_at: i64,
}

#[derive(Deserialize, Serialize)]
struct UsageFile {
    accounts: HashMap<String, AccountUsage>,
}

pub(super) struct UsageStore {
    path: PathBuf,
    accounts: Mutex<HashMap<String, AccountUsage>>,
}

pub(super) struct UsageLoad {
    pub(super) store: UsageStore,
    pub(super) warning: Option<String>,
}

impl UsageStore {
    pub(super) fn load(path: impl AsRef<Path>, now: i64) -> UsageLoad {
        let path = path.as_ref().to_path_buf();
        let mut warning = None;
        let mut accounts = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<UsageFile>(&bytes) {
                Ok(file) => file.accounts,
                Err(error) => {
                    warning = Some(format!(
                        "usage file {} is corrupt; ignoring saved usage: {error}",
                        path.display()
                    ));
                    HashMap::new()
                }
            },
            Err(error) if error.kind() == ErrorKind::NotFound => HashMap::new(),
            Err(error) => {
                warning = Some(format!(
                    "could not read usage file {}; ignoring saved usage: {error}",
                    path.display()
                ));
                HashMap::new()
            }
        };
        let before_retain = accounts.len();
        accounts.retain(|_, usage| usage.resets_at > now);
        let expired_usage_removed = before_retain != accounts.len();
        let store = Self {
            path,
            accounts: Mutex::new(accounts),
        };
        if expired_usage_removed && let Err(error) = store.persist() {
            warning = Some(format!("could not clear expired usage: {error}"));
        }
        UsageLoad { store, warning }
    }

    pub(super) fn get(&self, label: &str) -> Option<AccountUsage> {
        self.accounts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(label)
            .copied()
    }

    pub(super) fn record(&self, label: &str, usage: AccountUsage) -> Result<()> {
        self.accounts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(label.to_string(), usage);
        self.persist()
    }

    fn persist(&self) -> Result<()> {
        let accounts = self
            .accounts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let bytes = serde_json::to_vec(&UsageFile {
            accounts: accounts.clone(),
        })
        .context("could not serialize usage figures")?;
        fs::write(&self.path, bytes)
            .with_context(|| format!("could not write usage file {}", self.path.display()))
    }
}
