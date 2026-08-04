use pretty_assertions::assert_eq;
use std::path::PathBuf;

use super::PoolSettings;
use super::ProfileSettings;
use super::validate_label;
use super::default_listen_addr;
use super::default_upstream_base;

fn empty_pool() -> PoolSettings {
    PoolSettings {
        listen_addr: default_listen_addr(),
        upstream_base: default_upstream_base(),
        default_switch_at_percent: 80.0,
        profiles: Vec::new(),
    }
}

/// Creates a real directory under `temp` for `label` — needed by any test
/// that calls `PoolSettings::validate()`, since it bails if a profile's
/// `home` doesn't exist on disk.
fn account_dir(temp: &tempfile::TempDir, label: &str) -> PathBuf {
    let dir = temp.path().join(label);
    std::fs::create_dir_all(&dir).expect("create account directory");
    dir
}

#[test]
fn serialize_then_deserialize_round_trips() {
    let settings = PoolSettings {
        listen_addr: "127.0.0.1:8788".to_string(),
        upstream_base: "https://chatgpt.com/backend-api/codex".to_string(),
        default_switch_at_percent: 80.0,
        profiles: vec![ProfileSettings {
            label: "work".to_string(),
            home: "/tmp/work".into(),
            priority: 0,
            switch_at_percent: None,
            is_main: false,
        }],
    };

    let toml_text = toml::to_string_pretty(&settings).expect("serialize settings");
    let reloaded: PoolSettings = toml::from_str(&toml_text).expect("deserialize settings");

    assert_eq!(reloaded.listen_addr, settings.listen_addr);
    assert_eq!(reloaded.default_switch_at_percent, settings.default_switch_at_percent);
    assert_eq!(reloaded.profiles.len(), 1);
    assert_eq!(reloaded.profiles[0].label, "work");
    assert_eq!(reloaded.profiles[0].priority, 0);
}

#[test]
fn save_then_load_round_trips_through_a_real_file() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let path = temp.path().join("pool.toml");

    let settings = PoolSettings {
        listen_addr: "127.0.0.1:8788".to_string(),
        upstream_base: "https://chatgpt.com/backend-api/codex".to_string(),
        default_switch_at_percent: 80.0,
        profiles: vec![ProfileSettings {
            label: "work".to_string(),
            home: temp.path().join("work"),
            priority: 0,
            switch_at_percent: None,
            is_main: false,
        }],
    };

    settings.save(&path).expect("save settings");
    let reloaded = PoolSettings::load(&path).expect("reload saved settings");

    assert_eq!(reloaded.profiles.len(), 1);
    assert_eq!(reloaded.profiles[0].label, "work");
}

#[test]
fn ensure_exists_creates_a_starter_file_when_missing() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let path = temp.path().join("pool.toml");

    PoolSettings::ensure_exists(&path).expect("create starter pool.toml");

    let loaded = PoolSettings::load(&path).expect("load the created starter file");
    assert_eq!(loaded.default_switch_at_percent, 80.0);
    assert_eq!(loaded.profiles.len(), 0);
}

#[test]
fn ensure_exists_does_not_touch_an_existing_file() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let path = temp.path().join("pool.toml");
    std::fs::write(&path, "default_switch_at_percent = 55.0\nprofile = []\n")
        .expect("write existing file");

    PoolSettings::ensure_exists(&path).expect("ensure_exists on an existing file");

    let loaded = PoolSettings::load(&path).expect("load the untouched file");
    assert_eq!(loaded.default_switch_at_percent, 55.0);
}

#[test]
fn validate_label_accepts_simple_names() {
    assert!(validate_label("work").is_ok());
    assert!(validate_label("account-2").is_ok());
    assert!(validate_label("my_account").is_ok());
}

#[test]
fn validate_label_rejects_empty_and_bad_characters() {
    assert!(validate_label("").is_err());
    assert!(validate_label("work/account").is_err());
    assert!(validate_label("work account").is_err());
    assert!(validate_label("work.account").is_err());
}

#[test]
fn validate_label_rejects_reserved_windows_device_names_case_insensitively() {
    assert!(validate_label("CON").is_err());
    assert!(validate_label("con").is_err());
    assert!(validate_label("Nul").is_err());
    assert!(validate_label("COM1").is_err());
    assert!(validate_label("lpt9").is_err());
}

#[test]
fn upsert_adds_a_non_main_profile_at_priority_zero() {
    let mut pool = empty_pool();
    pool.upsert_profile("work", PathBuf::from("/tmp/work"), false)
        .expect("add work");

    assert_eq!(pool.profiles.len(), 1);
    assert_eq!(pool.profiles[0].label, "work");
    assert_eq!(pool.profiles[0].priority, 0);
    assert!(!pool.profiles[0].is_main);
}

#[test]
fn upsert_main_after_non_main_gets_the_highest_priority() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let mut pool = empty_pool();
    pool.upsert_profile("work", account_dir(&temp, "work"), false)
        .expect("add work");
    pool.upsert_profile("daily", account_dir(&temp, "daily"), true)
        .expect("add daily as main");

    let work = pool.profiles.iter().find(|p| p.label == "work").expect("work");
    let daily = pool.profiles.iter().find(|p| p.label == "daily").expect("daily");
    assert_eq!(work.priority, 0);
    assert_eq!(daily.priority, 1);
    assert!(daily.is_main);
    pool.validate().expect("recomputed priorities must satisfy validate()");
}

#[test]
fn upsert_main_before_non_main_is_recomputed_so_main_stays_highest() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let mut pool = empty_pool();
    pool.upsert_profile("daily", account_dir(&temp, "daily"), true)
        .expect("add daily as main first");
    pool.upsert_profile("work", account_dir(&temp, "work"), false)
        .expect("add work after main already exists");

    let work = pool.profiles.iter().find(|p| p.label == "work").expect("work");
    let daily = pool.profiles.iter().find(|p| p.label == "daily").expect("daily");
    assert_eq!(work.priority, 0);
    assert_eq!(daily.priority, 1, "main must be recomputed to stay the highest priority");
    pool.validate().expect("recomputed priorities must satisfy validate()");
}

#[test]
fn upsert_existing_label_updates_home_in_place_instead_of_duplicating() {
    let mut pool = empty_pool();
    pool.upsert_profile("work", PathBuf::from("/tmp/work-old"), false)
        .expect("add work");
    pool.upsert_profile("work", PathBuf::from("/tmp/work-new"), false)
        .expect("re-add work with a new home");

    assert_eq!(pool.profiles.len(), 1);
    assert_eq!(pool.profiles[0].home, PathBuf::from("/tmp/work-new"));
}

#[test]
fn upsert_rejects_case_only_label_collision() {
    let mut pool = empty_pool();
    pool.upsert_profile("Work", PathBuf::from("/tmp/work"), false)
        .expect("add Work");

    let error = pool
        .upsert_profile("work", PathBuf::from("/tmp/other"), false)
        .expect_err("case-only collision must be rejected");
    assert!(error.to_string().contains("work"));
}

#[test]
fn upsert_rejects_invalid_label() {
    let mut pool = empty_pool();
    let error = pool
        .upsert_profile("bad label", PathBuf::from("/tmp/bad"), false)
        .expect_err("invalid label must be rejected");
    assert!(error.to_string().contains("letters, digits"));
}

#[test]
fn upsert_new_main_demotes_the_previous_main() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let mut pool = empty_pool();
    pool.upsert_profile("old-daily", account_dir(&temp, "old-daily"), true)
        .expect("add old-daily as main");
    pool.upsert_profile("new-daily", account_dir(&temp, "new-daily"), true)
        .expect("add new-daily as main");

    let old = pool.profiles.iter().find(|p| p.label == "old-daily").expect("old-daily");
    let new = pool.profiles.iter().find(|p| p.label == "new-daily").expect("new-daily");
    assert!(!old.is_main, "only one profile may be main");
    assert!(new.is_main);
    pool.validate().expect("recomputed priorities must satisfy validate()");
}
