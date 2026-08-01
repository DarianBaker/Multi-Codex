use std::path::Path;

use crate::PoolSettings;
use crate::usage::UsageStore;

/// Text and optional load warning produced by the live pool status command.
pub struct PoolStatus {
    pub output: String,
    pub warning: Option<String>,
}

/// Reads saved live usage and formats one status line per configured account.
pub fn pool_status(settings: &PoolSettings, usage_path: impl AsRef<Path>, now: i64) -> PoolStatus {
    let loaded = UsageStore::load(usage_path, now);
    let (usage_by_account, paying_account) = loaded.store.snapshot();
    let mut output = String::new();
    let mut total_used = 0.0;
    let mut reported_accounts = 0;

    for profile in &settings.profiles {
        let marker = if paying_account.as_deref() == Some(&profile.label) {
            "* "
        } else {
            "  "
        };
        let paying = if marker == "* " { " [PAYING]" } else { "" };
        if let Some(usage) = usage_by_account.get(&profile.label) {
            total_used += usage.used_percent;
            reported_accounts += 1;
            if usage.used_percent >= settings.switch_at_percent_for(profile) {
                output.push_str(&format!(
                    "{marker}{}{paying}: {:.1}% used; EXHAUSTED until {} (Unix seconds)\n",
                    profile.label, usage.used_percent, usage.resets_at
                ));
            } else {
                output.push_str(&format!(
                    "{marker}{}{paying}: {:.1}% used; resets at {} (Unix seconds)\n",
                    profile.label, usage.used_percent, usage.resets_at
                ));
            }
        } else {
            output.push_str(&format!(
                "{marker}{}{paying}: usage unavailable; reset unavailable\n",
                profile.label
            ));
        }
    }

    if reported_accounts == settings.profiles.len() && reported_accounts > 0 {
        let used_percent = total_used / reported_accounts as f64;
        output.push_str(&format!(
            "POOL TOTAL: {used_percent:.1}% used; {:.1}% remaining across {reported_accounts} accounts\n",
            100.0 - used_percent
        ));
    } else {
        output.push_str(&format!(
            "POOL TOTAL: unavailable; usage reported for {reported_accounts} of {} accounts\n",
            settings.profiles.len()
        ));
    }

    PoolStatus {
        output,
        warning: loaded.warning,
    }
}
