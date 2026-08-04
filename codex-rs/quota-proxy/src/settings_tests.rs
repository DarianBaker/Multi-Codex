use pretty_assertions::assert_eq;

use super::PoolSettings;
use super::ProfileSettings;

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
