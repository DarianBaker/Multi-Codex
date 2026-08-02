use std::path::Path;

use pretty_assertions::assert_eq;

use super::CheckOutcome;
use super::SelfcheckReport;
use super::current_paying_account;
use super::force_account_over_threshold;
use super::throwaway_usage_path;
use crate::usage::UsageStore;

#[test]
fn throwaway_usage_path_is_derived_from_the_settings_path() {
    let path = throwaway_usage_path(Path::new("/pool/settings.toml"));
    assert_eq!(path, Path::new("/pool/settings.selfcheck.usage.json"));
}

#[test]
fn force_account_over_threshold_then_current_paying_account_round_trip() {
    let temp = tempfile::tempdir().expect("create temporary usage directory");
    let usage_path = temp.path().join("pool.selfcheck.usage.json");
    let loaded = UsageStore::load(&usage_path, 0);
    loaded
        .store
        .set_paying_account("acctX")
        .expect("mark acctX as paying");

    assert_eq!(
        current_paying_account(&usage_path),
        Some("acctX".to_string())
    );

    force_account_over_threshold(&usage_path, "acctX").expect("force acctX over its threshold");

    let reloaded = UsageStore::load(&usage_path, 0);
    let usage = reloaded
        .store
        .get("acctX")
        .expect("acctX has a forced usage record");
    assert_eq!(usage.used_percent, 100.0);
}

#[test]
fn render_reports_pass_when_every_check_is_ok() {
    let report = SelfcheckReport {
        redirection: CheckOutcome::Ok("reached real upstream, status 200 OK".to_string()),
        account_swap: CheckOutcome::Ok("acctX -> acctY".to_string()),
        usage_reading: CheckOutcome::Ok("3 rows (2 account(s) + pool total)".to_string()),
        elapsed: std::time::Duration::from_secs_f64(8.2),
    };

    assert!(report.passed());
    let rendered = report.render();
    assert_eq!(
        rendered,
        "REDIRECTION: OK (reached real upstream, status 200 OK)\n\
ACCOUNT SWAP: OK (acctX -> acctY)\n\
USAGE READING: OK (3 rows (2 account(s) + pool total))\n\
SELFCHECK: PASS (0 of 3 checks broken, 8.2s)\n"
    );
}

#[test]
fn render_names_exactly_which_checks_broke() {
    let report = SelfcheckReport {
        redirection: CheckOutcome::Ok("reached real upstream, status 200 OK".to_string()),
        account_swap: CheckOutcome::Broken(
            "both calls paid by 'acctX'; expected a different account".to_string(),
        ),
        usage_reading: CheckOutcome::Broken(
            "found 2 row(s), expected 4 (3 account(s) + 1 pool total)".to_string(),
        ),
        elapsed: std::time::Duration::from_secs_f64(6.1),
    };

    assert!(!report.passed());
    let rendered = report.render();
    assert!(rendered.contains("ACCOUNT SWAP: BROKEN (both calls paid by 'acctX'"));
    assert!(rendered.contains("USAGE READING: BROKEN (found 2 row(s)"));
    assert!(rendered.contains("SELFCHECK: FAIL (2 of 3 checks broken, 6.1s)"));
}

#[test]
fn skipped_account_swap_does_not_count_as_broken() {
    let report = SelfcheckReport {
        redirection: CheckOutcome::Ok("reached real upstream, status 200 OK".to_string()),
        account_swap: CheckOutcome::Skipped("only 1 account configured".to_string()),
        usage_reading: CheckOutcome::Ok("2 rows (1 account(s) + pool total)".to_string()),
        elapsed: std::time::Duration::from_secs_f64(1.0),
    };

    assert!(report.passed());
    assert!(
        report
            .render()
            .contains("ACCOUNT SWAP: SKIPPED (only 1 account configured)")
    );
}
