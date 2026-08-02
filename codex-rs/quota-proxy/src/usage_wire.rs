use codex_backend_openapi_models::models::AdditionalRateLimitDetails;
use codex_backend_openapi_models::models::PlanType;
use codex_backend_openapi_models::models::RateLimitStatusDetails;
use codex_backend_openapi_models::models::RateLimitStatusPayload;
use codex_backend_openapi_models::models::RateLimitWindowSnapshot;

use crate::usage::AccountUsage;

/// One row to render in the usage screen: a configured account's label plus its
/// most recently known usage, if any is known yet.
pub(crate) struct AccountUsageRow {
    pub(crate) label: String,
    pub(crate) limit_id: String,
    pub(crate) usage: Option<AccountUsage>,
    pub(crate) is_paying: bool,
}

/// Builds the synthesized `/api/codex/usage`-shaped JSON body: the top-level
/// "codex" family carries `paying_row`'s usage (preserving today's single-account
/// behavior), and every row in `rows` (including the paying one) becomes its own
/// named `additional_rate_limits` entry.
pub(crate) fn synthesize_usage_response(
    plan_type: PlanType,
    paying_row: &AccountUsageRow,
    rows: &[AccountUsageRow],
    now: i64,
) -> Vec<u8> {
    let payload = RateLimitStatusPayload {
        plan_type,
        rate_limit: Some(Some(Box::new(account_usage_to_details(
            paying_row.usage,
            now,
        )))),
        credits: None,
        spend_control: None,
        additional_rate_limits: Some(Some(
            rows.iter()
                .map(|row| account_row_to_additional(row, now))
                .collect(),
        )),
        rate_limit_reached_type: None,
    };
    serde_json::to_vec(&payload).unwrap_or_default()
}

fn account_row_to_additional(row: &AccountUsageRow, now: i64) -> AdditionalRateLimitDetails {
    let limit_name = if row.is_paying {
        format!("{} (paying)", row.label)
    } else {
        row.label.to_string()
    };
    AdditionalRateLimitDetails {
        limit_name,
        metered_feature: row.limit_id.clone(),
        rate_limit: Some(Some(Box::new(account_usage_to_details(row.usage, now)))),
    }
}

fn account_usage_to_details(usage: Option<AccountUsage>, now: i64) -> RateLimitStatusDetails {
    match usage {
        Some(usage) => RateLimitStatusDetails {
            allowed: usage.used_percent < 100.0,
            limit_reached: usage.used_percent >= 100.0,
            primary_window: Some(Some(Box::new(RateLimitWindowSnapshot {
                used_percent: usage.used_percent.round() as i32,
                limit_window_seconds: (usage.window_minutes * 60) as i32,
                reset_after_seconds: (usage.resets_at - now).max(0) as i32,
                reset_at: usage.resets_at as i32,
            }))),
            secondary_window: None,
        },
        None => RateLimitStatusDetails {
            allowed: true,
            limit_reached: false,
            primary_window: None,
            secondary_window: None,
        },
    }
}

/// Builds an account's `limit_id`. The main account always sorts first among the
/// per-account rows (the TUI renders families in `limit_id` order), regardless of
/// its label; every other account keeps its label-derived id, so their relative
/// order is unspecified.
pub(crate) fn account_limit_id(label: &str, is_main: bool) -> String {
    if is_main {
        "pool:0-main".to_string()
    } else {
        format!("pool:{label}")
    }
}

/// Builds the synthetic "Pool total" row: the average `used_percent` across every
/// account with known usage (accounts with no known usage yet are excluded from the
/// average rather than treated as 0% used), and the earliest `resets_at` among
/// those known accounts. `usage` is `None` when no account has known usage yet.
pub(crate) fn pool_total_row(rows: &[AccountUsageRow]) -> AccountUsageRow {
    let known: Vec<AccountUsage> = rows.iter().filter_map(|row| row.usage).collect();
    let usage = if known.is_empty() {
        None
    } else {
        let average =
            known.iter().map(|usage| usage.used_percent).sum::<f64>() / known.len() as f64;
        let earliest_reset = known.iter().map(|usage| usage.resets_at).min().unwrap_or(0);
        Some(AccountUsage {
            used_percent: average,
            window_minutes: known[0].window_minutes,
            resets_at: earliest_reset,
        })
    };
    AccountUsageRow {
        label: "Pool total".to_string(),
        limit_id: "pool:total".to_string(),
        usage,
        is_paying: false,
    }
}

/// Verifies a synthesized `/api/codex/usage` response has exactly one row per
/// configured account plus the pool-total row, each with a non-blank label.
/// Returns the row count on success, or a human-readable description of what's
/// wrong (used verbatim in `selfcheck`'s `USAGE READING: BROKEN (...)` line).
pub(crate) fn verify_usage_reading(
    body: &[u8],
    expected_account_count: usize,
) -> Result<usize, String> {
    let payload: RateLimitStatusPayload = serde_json::from_slice(body)
        .map_err(|error| format!("response is not valid JSON: {error}"))?;
    let rows = payload
        .additional_rate_limits
        .flatten()
        .ok_or_else(|| "response has no additional_rate_limits at all".to_string())?;

    let expected_rows = expected_account_count + 1;
    if rows.len() != expected_rows {
        return Err(format!(
            "found {} row(s), expected {expected_rows} ({expected_account_count} account(s) + 1 pool total)",
            rows.len()
        ));
    }
    if let Some(blank_index) = rows.iter().position(|row| row.limit_name.trim().is_empty()) {
        return Err(format!("row {blank_index} has a blank label"));
    }
    Ok(rows.len())
}

/// Parses a real upstream `/api/codex/usage`-shaped body into the plan type and
/// primary-window usage for that one account. Returns `None` on any parse failure
/// or when the payload carries no primary window (both treated as "unknown usage"
/// by callers, never as an error).
pub(crate) fn parse_primary_usage(body: &[u8]) -> Option<(PlanType, AccountUsage)> {
    let payload: RateLimitStatusPayload = serde_json::from_slice(body).ok()?;
    let window = payload
        .rate_limit
        .clone()
        .flatten()?
        .primary_window
        .flatten()?;
    let window_minutes = if window.limit_window_seconds > 0 {
        (i64::from(window.limit_window_seconds) + 59) / 60
    } else {
        0
    };
    Some((
        payload.plan_type,
        AccountUsage {
            used_percent: f64::from(window.used_percent),
            window_minutes,
            resets_at: i64::from(window.reset_at),
        },
    ))
}

#[cfg(test)]
mod tests {
    use crate::usage::AccountUsage;
    use crate::usage_wire::AccountUsageRow;
    use crate::usage_wire::synthesize_usage_response;
    use codex_backend_openapi_models::models::PlanType;
    use codex_backend_openapi_models::models::RateLimitStatusPayload;
    use pretty_assertions::assert_eq;

    fn usage(used_percent: f64, resets_at: i64) -> AccountUsage {
        AccountUsage {
            used_percent,
            window_minutes: 300,
            resets_at,
        }
    }

    #[test]
    fn main_account_limit_id_sorts_before_any_other_account_label() {
        use crate::usage_wire::account_limit_id;

        // Deliberately alphabetically-last label: proves the ordering comes from
        // `is_main`, not from the label text.
        let main_id = account_limit_id("Zzz Main", true);
        let other_id = account_limit_id("Aaa Other", false);

        assert!(
            main_id < other_id,
            "main account's limit_id must sort first regardless of label"
        );
    }

    #[test]
    fn non_main_account_limit_id_is_unchanged() {
        use crate::usage_wire::account_limit_id;

        assert_eq!(account_limit_id("Pool B", false), "pool:Pool B");
    }

    #[test]
    fn verify_usage_reading_accepts_one_row_per_account_plus_total() {
        use crate::usage_wire::pool_total_row;
        use crate::usage_wire::verify_usage_reading;

        let rows = vec![
            AccountUsageRow {
                label: "Pool A".to_string(),
                limit_id: "pool:Pool A".to_string(),
                usage: Some(usage(10.0, 1_700_000_300)),
                is_paying: true,
            },
            AccountUsageRow {
                label: "Pool B".to_string(),
                limit_id: "pool:Pool B".to_string(),
                usage: None,
                is_paying: false,
            },
        ];
        let total = pool_total_row(&rows);
        let all_rows: Vec<AccountUsageRow> =
            rows.into_iter().chain(std::iter::once(total)).collect();
        let body =
            synthesize_usage_response(PlanType::Plus, &all_rows[0], &all_rows, 1_700_000_000);

        let count = verify_usage_reading(&body, 2).expect("two accounts plus total should verify");
        assert_eq!(count, 3);
    }

    #[test]
    fn verify_usage_reading_reports_missing_rows() {
        use crate::usage_wire::verify_usage_reading;

        let rows = vec![AccountUsageRow {
            label: "Pool A".to_string(),
            limit_id: "pool:Pool A".to_string(),
            usage: Some(usage(10.0, 1_700_000_300)),
            is_paying: true,
        }];
        let body = synthesize_usage_response(PlanType::Plus, &rows[0], &rows, 1_700_000_000);

        let error = verify_usage_reading(&body, 2)
            .expect_err("only 1 of 2 accounts present, plus no total row");
        assert!(
            error.contains("1") && error.contains("3"),
            "error should mention both the actual and expected row counts: {error}"
        );
    }

    #[test]
    fn verify_usage_reading_reports_a_blank_label() {
        use crate::usage_wire::verify_usage_reading;

        let rows = vec![
            AccountUsageRow {
                label: String::new(),
                limit_id: "pool:blank".to_string(),
                usage: Some(usage(10.0, 1_700_000_300)),
                is_paying: false,
            },
            AccountUsageRow {
                label: "Pool total".to_string(),
                limit_id: "pool:total".to_string(),
                usage: Some(usage(10.0, 1_700_000_300)),
                is_paying: false,
            },
        ];
        let body = synthesize_usage_response(PlanType::Plus, &rows[0], &rows, 1_700_000_000);

        let error =
            verify_usage_reading(&body, 1).expect_err("a blank row label should be reported");
        assert!(
            error.contains("blank"),
            "error should call out the blank label: {error}"
        );
    }

    #[test]
    fn pool_total_row_averages_known_usage_and_uses_earliest_reset() {
        use crate::usage_wire::pool_total_row;

        let rows = vec![
            AccountUsageRow {
                label: "Pool A".to_string(),
                limit_id: "pool:Pool A".to_string(),
                usage: Some(usage(40.0, 1_700_000_300)),
                is_paying: true,
            },
            AccountUsageRow {
                label: "Pool B".to_string(),
                limit_id: "pool:Pool B".to_string(),
                usage: Some(usage(20.0, 1_700_000_100)),
                is_paying: false,
            },
            AccountUsageRow {
                label: "Pool C".to_string(),
                limit_id: "pool:Pool C".to_string(),
                usage: None,
                is_paying: false,
            },
        ];

        let total = pool_total_row(&rows);

        assert_eq!(total.label, "Pool total");
        assert!(!total.is_paying);
        let total_usage = total
            .usage
            .expect("total has usage once any account is known");
        assert_eq!(total_usage.used_percent, 30.0);
        assert_eq!(total_usage.resets_at, 1_700_000_100);
    }

    #[test]
    fn pool_total_row_is_unknown_when_no_account_usage_is_known() {
        use crate::usage_wire::pool_total_row;

        let rows = vec![AccountUsageRow {
            label: "Pool A".to_string(),
            limit_id: "pool:Pool A".to_string(),
            usage: None,
            is_paying: true,
        }];

        let total = pool_total_row(&rows);

        assert!(total.usage.is_none());
    }

    #[test]
    fn parses_primary_usage_from_upstream_payload() {
        use crate::usage_wire::parse_primary_usage;
        use codex_backend_openapi_models::models::RateLimitStatusDetails;
        use codex_backend_openapi_models::models::RateLimitWindowSnapshot;

        let body = serde_json::to_vec(&RateLimitStatusPayload {
            plan_type: PlanType::Plus,
            rate_limit: Some(Some(Box::new(RateLimitStatusDetails {
                allowed: true,
                limit_reached: false,
                primary_window: Some(Some(Box::new(RateLimitWindowSnapshot {
                    used_percent: 37,
                    limit_window_seconds: 18000,
                    reset_after_seconds: 1000,
                    reset_at: 1_700_000_500,
                }))),
                secondary_window: None,
            }))),
            credits: None,
            spend_control: None,
            additional_rate_limits: None,
            rate_limit_reached_type: None,
        })
        .expect("serialize fixture payload");

        let (plan_type, usage) = parse_primary_usage(&body).expect("parses upstream payload");

        assert_eq!(plan_type, PlanType::Plus);
        assert_eq!(
            usage,
            AccountUsage {
                used_percent: 37.0,
                window_minutes: 300,
                resets_at: 1_700_000_500,
            }
        );
    }

    #[test]
    fn returns_none_for_an_unparseable_body() {
        use crate::usage_wire::parse_primary_usage;

        assert!(parse_primary_usage(b"not json").is_none());
    }

    #[test]
    fn synthesizes_one_row_per_account_and_marks_the_payer() {
        let rows = vec![
            AccountUsageRow {
                label: "Pool A".to_string(),
                limit_id: "pool:Pool A".to_string(),
                usage: Some(usage(42.0, 1_700_000_300)),
                is_paying: true,
            },
            AccountUsageRow {
                label: "Pool B".to_string(),
                limit_id: "pool:Pool B".to_string(),
                usage: Some(usage(10.0, 1_700_000_600)),
                is_paying: false,
            },
        ];
        let body =
            synthesize_usage_response(PlanType::Plus, &rows[0], &rows, /*now*/ 1_700_000_000);
        let payload: RateLimitStatusPayload = serde_json::from_slice(&body)
            .expect("synthesized body parses as RateLimitStatusPayload");

        let additional = payload
            .additional_rate_limits
            .flatten()
            .expect("additional_rate_limits present");
        let names: Vec<&str> = additional
            .iter()
            .map(|entry| entry.limit_name.as_str())
            .collect();
        assert_eq!(names, vec!["Pool A (paying)", "Pool B"]);

        let paying_details = additional[0]
            .rate_limit
            .clone()
            .flatten()
            .expect("paying account has rate limit details");
        let paying_primary = paying_details
            .primary_window
            .flatten()
            .expect("paying account has a primary window");
        assert_eq!(paying_primary.used_percent, 42);
        assert_eq!(paying_primary.reset_at, 1_700_000_300);

        // Main/"codex" family (today's default, unlabeled row) is preserved.
        let codex_details = payload
            .rate_limit
            .flatten()
            .expect("top-level codex family present");
        let codex_primary = codex_details
            .primary_window
            .flatten()
            .expect("codex family has a primary window");
        assert_eq!(codex_primary.used_percent, 42);
        assert_eq!(codex_primary.reset_at, 1_700_000_300);
    }

    #[test]
    fn account_with_unknown_usage_still_gets_a_row_without_fabricating_data() {
        let rows = vec![
            AccountUsageRow {
                label: "Pool A".to_string(),
                limit_id: "pool:Pool A".to_string(),
                usage: Some(usage(5.0, 1_700_000_300)),
                is_paying: true,
            },
            AccountUsageRow {
                label: "Pool B".to_string(),
                limit_id: "pool:Pool B".to_string(),
                usage: None,
                is_paying: false,
            },
        ];
        let body =
            synthesize_usage_response(PlanType::Plus, &rows[0], &rows, /*now*/ 1_700_000_000);
        let payload: RateLimitStatusPayload =
            serde_json::from_slice(&body).expect("synthesized body parses");

        let additional = payload
            .additional_rate_limits
            .flatten()
            .expect("additional_rate_limits present");
        assert_eq!(additional.len(), 2);
        assert_eq!(additional[1].limit_name, "Pool B");

        let unknown_has_primary_window = additional[1]
            .rate_limit
            .clone()
            .flatten()
            .and_then(|details| details.primary_window.flatten())
            .is_some();
        assert!(
            !unknown_has_primary_window,
            "an account with no known usage yet must not fabricate a primary window"
        );
    }
}
