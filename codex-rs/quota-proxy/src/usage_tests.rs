use std::fs;

use pretty_assertions::assert_eq;
use serde_json::json;

use crate::usage::AccountUsage;
use crate::usage::UsageStore;

#[test]
fn usage_changes_are_saved_to_file() {
    let temp = tempfile::tempdir().expect("create temporary usage directory");
    let path = temp.path().join("pool.usage.json");
    let loaded = UsageStore::load(&path, 100);
    loaded
        .store
        .record(
            "Pool B",
            AccountUsage {
                used_percent: 20.0,
                window_minutes: 300,
                resets_at: 200,
            },
        )
        .expect("save first usage change");
    let first: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).expect("read usage file after first change"))
            .expect("parse usage file after first change");
    assert_eq!(first["accounts"]["Pool B"]["used_percent"], json!(20.0));

    loaded
        .store
        .record(
            "Pool B",
            AccountUsage {
                used_percent: 35.0,
                window_minutes: 300,
                resets_at: 250,
            },
        )
        .expect("save second usage change");
    let second: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).expect("read usage file after second change"))
            .expect("parse usage file after second change");
    assert_eq!(second["accounts"]["Pool B"]["used_percent"], json!(35.0));
}

#[test]
fn saved_usage_is_loaded() {
    let temp = tempfile::tempdir().expect("create temporary usage directory");
    let path = temp.path().join("pool.usage.json");
    fs::write(
        &path,
        r#"{"accounts":{"Pool B":{"used_percent":42.0,"window_minutes":300,"resets_at":200}}}"#,
    )
    .expect("write saved usage fixture");

    let loaded = UsageStore::load(&path, 100);

    assert_eq!(loaded.warning, None);
    assert_eq!(
        loaded.store.get("Pool B"),
        Some(AccountUsage {
            used_percent: 42.0,
            window_minutes: 300,
            resets_at: 200,
        })
    );
}

#[test]
fn expired_usage_is_cleared_on_load() {
    let temp = tempfile::tempdir().expect("create temporary usage directory");
    let path = temp.path().join("pool.usage.json");
    fs::write(
        &path,
        r#"{"accounts":{"Expired":{"used_percent":90.0,"window_minutes":300,"resets_at":100},"Current":{"used_percent":10.0,"window_minutes":300,"resets_at":101}}}"#,
    )
    .expect("write saved usage fixture");

    let loaded = UsageStore::load(&path, 100);
    let persisted: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).expect("read cleared usage file"))
            .expect("parse cleared usage file");

    assert_eq!(loaded.store.get("Expired"), None);
    assert_eq!(
        loaded.store.get("Current"),
        Some(AccountUsage {
            used_percent: 10.0,
            window_minutes: 300,
            resets_at: 101,
        })
    );
    assert_eq!(persisted["accounts"].get("Expired"), None);
}

#[test]
fn corrupt_usage_file_is_reported_and_ignored() {
    let temp = tempfile::tempdir().expect("create temporary usage directory");
    let path = temp.path().join("pool.usage.json");
    fs::write(&path, b"{not json").expect("write corrupt usage fixture");

    let loaded = UsageStore::load(&path, 100);

    assert!(
        loaded
            .warning
            .as_deref()
            .is_some_and(|warning| warning.contains("is corrupt; ignoring saved usage"))
    );
    assert_eq!(loaded.store.get("Pool B"), None);
}
