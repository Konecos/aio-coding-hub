//! Usage: Runtime cache and gateway gating for OAuth provider quota snapshots.

use crate::db;
use crate::shared::error::{db_err, AppError, AppResult};
use crate::shared::time::now_unix_seconds;
use rusqlite::{params, Connection, OptionalExtension};

const TEXT_MAX_CHARS: usize = 96;
const SHORT_LABEL_MAX_CHARS: usize = 32;
const FALLBACK_COOLDOWN_SECS: i64 = 5 * 60;
pub(crate) const SNAPSHOT_MAX_AGE_SECS: i64 = 120;

#[derive(Debug, Clone)]
pub(crate) struct OAuthLimitSnapshotInput<'a> {
    pub provider_id: i64,
    pub short_remaining_percent: Option<f64>,
    pub long_remaining_percent: Option<f64>,
    pub limit_short_label: Option<&'a str>,
    pub limit_5h_text: Option<&'a str>,
    pub limit_weekly_text: Option<&'a str>,
    pub limit_5h_reset_at: Option<i64>,
    pub limit_weekly_reset_at: Option<i64>,
    pub reset_credit_available_count: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OAuthLimitGate {
    Allow,
    Limited { reset_at: Option<i64> },
    Threshold { reset_at: Option<i64> },
    Unverified,
}

#[derive(Debug, Clone)]
pub(crate) struct OAuthLimitSnapshot {
    pub short_remaining_percent: Option<f64>,
    pub long_remaining_percent: Option<f64>,
    pub limit_5h_text: Option<String>,
    pub limit_weekly_text: Option<String>,
    pub limit_5h_reset_at: Option<i64>,
    pub limit_weekly_reset_at: Option<i64>,
    pub reset_credit_available_count: Option<i64>,
    pub checked_at: i64,
}

fn validate_provider_id(provider_id: i64) -> AppResult<i64> {
    if provider_id <= 0 {
        return Err(AppError::from(format!(
            "SEC_INVALID_INPUT: invalid provider_id={provider_id}"
        )));
    }
    Ok(provider_id)
}

fn take_first_chars(value: &str, max_chars: usize) -> String {
    if value.chars().nth(max_chars).is_none() {
        return value.to_string();
    }
    value.chars().take(max_chars).collect()
}

fn normalize_text(input: Option<&str>, max_chars: usize) -> Option<String> {
    let value = input.map(str::trim).filter(|value| !value.is_empty())?;
    Some(take_first_chars(value, max_chars))
}

fn normalize_reset_at(input: Option<i64>) -> Option<i64> {
    input.filter(|value| *value > 0)
}

fn normalize_reset_credit_available_count(input: Option<i64>) -> Option<i64> {
    input.filter(|value| *value >= 0)
}

fn update_latest(latest: &mut Option<i64>, candidate: i64) {
    if candidate <= 0 {
        return;
    }
    match latest {
        Some(existing) if *existing >= candidate => {}
        _ => *latest = Some(candidate),
    }
}

fn parse_leading_number(text: &str) -> Option<(f64, &str)> {
    let mut end = 0usize;
    let mut seen_digit = false;
    let mut seen_dot = false;

    for (idx, ch) in text.char_indices() {
        if ch.is_ascii_digit() {
            seen_digit = true;
            end = idx + ch.len_utf8();
            continue;
        }
        if ch == '.' && !seen_dot {
            seen_dot = true;
            end = idx + ch.len_utf8();
            continue;
        }
        break;
    }

    if !seen_digit || end == 0 {
        return None;
    }

    let number = text[..end].parse::<f64>().ok()?;
    Some((number, &text[end..]))
}

fn is_exhausted_quota_text(input: Option<&str>) -> bool {
    let Some(text) = input.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    let normalized = text.replace(',', "");
    let Some((value, rest)) = parse_leading_number(&normalized) else {
        return false;
    };

    if value.abs() > f64::EPSILON {
        return false;
    }

    let rest = rest.trim_start();
    let starts_with_unit = rest
        .chars()
        .next()
        .is_some_and(|ch| ch == '%' || ch == '/' || ch.is_alphabetic());
    rest.is_empty() || starts_with_unit
}

fn active_exhausted_window_reset_at(
    text: Option<&str>,
    reset_at: Option<i64>,
    checked_at: i64,
    now_unix: i64,
) -> Option<i64> {
    if !is_exhausted_quota_text(text) {
        return None;
    }

    if let Some(reset_at) = reset_at {
        return (reset_at > now_unix).then_some(reset_at);
    }

    let fallback_until = checked_at.saturating_add(FALLBACK_COOLDOWN_SECS);
    (fallback_until > now_unix).then_some(fallback_until)
}

#[cfg(test)]
pub(crate) fn save_snapshot(db: &db::Db, input: OAuthLimitSnapshotInput<'_>) -> AppResult<()> {
    let conn = db.open_connection()?;
    save_snapshot_on(&conn, input)
}

fn save_snapshot_on(conn: &Connection, input: OAuthLimitSnapshotInput<'_>) -> AppResult<()> {
    let provider_id = validate_provider_id(input.provider_id)?;
    let now = now_unix_seconds();
    let limit_short_label = normalize_text(input.limit_short_label, SHORT_LABEL_MAX_CHARS);
    let limit_5h_text = normalize_text(input.limit_5h_text, TEXT_MAX_CHARS);
    let limit_weekly_text = normalize_text(input.limit_weekly_text, TEXT_MAX_CHARS);
    let limit_5h_reset_at = normalize_reset_at(input.limit_5h_reset_at);
    let limit_weekly_reset_at = normalize_reset_at(input.limit_weekly_reset_at);
    let reset_credit_available_count =
        normalize_reset_credit_available_count(input.reset_credit_available_count);

    conn.execute(
        r#"
INSERT INTO provider_oauth_limit_snapshots(
  provider_id,
  short_remaining_percent,
  long_remaining_percent,
  limit_short_label,
  limit_5h_text,
  limit_weekly_text,
  limit_5h_reset_at,
  limit_weekly_reset_at,
  reset_credit_available_count,
  checked_at,
  updated_at
) VALUES (?1, ?9, ?10, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
ON CONFLICT(provider_id) DO UPDATE SET
  short_remaining_percent = excluded.short_remaining_percent,
  long_remaining_percent = excluded.long_remaining_percent,
  limit_short_label = excluded.limit_short_label,
  limit_5h_text = excluded.limit_5h_text,
  limit_weekly_text = excluded.limit_weekly_text,
  limit_5h_reset_at = excluded.limit_5h_reset_at,
  limit_weekly_reset_at = excluded.limit_weekly_reset_at,
  reset_credit_available_count = excluded.reset_credit_available_count,
  checked_at = excluded.checked_at,
  updated_at = excluded.updated_at
"#,
        params![
            provider_id,
            limit_short_label,
            limit_5h_text,
            limit_weekly_text,
            limit_5h_reset_at,
            limit_weekly_reset_at,
            reset_credit_available_count,
            now,
            normalize_percent(input.short_remaining_percent),
            normalize_percent(input.long_remaining_percent)
        ],
    )
    .map_err(|e| db_err!("failed to save OAuth limit snapshot: {e}"))?;

    Ok(())
}

pub(crate) fn save_exhausted_snapshot(
    db: &db::Db,
    provider_id: i64,
    reset_at: Option<i64>,
) -> AppResult<()> {
    let now = now_unix_seconds();
    let mut conn = db.open_connection()?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| db_err!("failed to start exhaustion save: {e}"))?;
    let existing_snapshot = read_snapshot(&tx, provider_id)?;
    tx.execute(
        "UPDATE providers SET oauth_quota_generation = oauth_quota_generation + 1 WHERE id = ?1",
        params![provider_id],
    )
    .map_err(|e| db_err!("failed to advance quota generation: {e}"))?;
    let effective_reset_at = match reset_at {
        Some(reset_at) => Some(reset_at),
        None => existing_snapshot.as_ref().and_then(|snapshot| {
            [snapshot.limit_5h_reset_at, snapshot.limit_weekly_reset_at]
                .into_iter()
                .flatten()
                .filter(|candidate| *candidate > now)
                .max()
        }),
    };
    let reset_credit_available_count = existing_snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.reset_credit_available_count);

    save_snapshot_on(
        &tx,
        OAuthLimitSnapshotInput {
            provider_id,
            short_remaining_percent: Some(0.0),
            long_remaining_percent: None,
            limit_short_label: None,
            limit_5h_text: Some("0"),
            limit_weekly_text: None,
            limit_5h_reset_at: effective_reset_at,
            limit_weekly_reset_at: None,
            reset_credit_available_count,
        },
    )?;
    tx.commit()
        .map_err(|e| db_err!("failed to commit exhaustion snapshot: {e}"))?;
    Ok(())
}

pub(crate) fn clear_snapshot_on(conn: &Connection, provider_id: i64) -> AppResult<()> {
    let provider_id = validate_provider_id(provider_id)?;
    conn.execute(
        "UPDATE providers SET oauth_quota_generation = oauth_quota_generation + 1 WHERE id = ?1",
        params![provider_id],
    )
    .map_err(|e| db_err!("failed to invalidate quota generation: {e}"))?;
    conn.execute(
        "DELETE FROM provider_oauth_limit_snapshots WHERE provider_id = ?1",
        params![provider_id],
    )
    .map_err(|e| db_err!("failed to clear OAuth limit snapshot: {e}"))?;
    conn.execute(
        "DELETE FROM provider_oauth_quota_refresh WHERE provider_id = ?1",
        params![provider_id],
    )
    .map_err(|e| db_err!("failed to clear quota refresh state: {e}"))?;
    Ok(())
}

pub(crate) fn generation(db: &db::Db, provider_id: i64) -> AppResult<i64> {
    let conn = db.open_connection()?;
    conn.query_row(
        "SELECT oauth_quota_generation FROM providers WHERE id = ?1",
        params![provider_id],
        |row| row.get(0),
    )
    .map_err(|e| db_err!("failed to read quota generation: {e}"))
}

pub(crate) fn save_snapshot_if_current(
    db: &db::Db,
    input: OAuthLimitSnapshotInput<'_>,
    generation: i64,
) -> AppResult<()> {
    let mut conn = db.open_connection()?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| db_err!("failed to start quota save: {e}"))?;
    let current: Option<i64> = tx
        .query_row(
            "SELECT oauth_quota_generation FROM providers WHERE id = ?1 AND auth_mode = 'oauth'",
            params![input.provider_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| db_err!("failed to check quota generation: {e}"))?;
    if current != Some(generation) {
        return Err("OAUTH_QUOTA_SUPERSEDED: provider quota state changed during refresh".into());
    }
    let provider_id = input.provider_id;
    save_snapshot_on(&tx, input)?;
    // A successful reset confirmation also clears earlier refresh backoff and errors.
    let now = now_unix_seconds();
    tx.execute(
        "INSERT INTO provider_oauth_quota_refresh(provider_id, last_attempt_at, next_attempt_at, failures, last_error) VALUES (?1, ?2, ?3, 0, NULL) ON CONFLICT(provider_id) DO UPDATE SET last_attempt_at = excluded.last_attempt_at, next_attempt_at = excluded.next_attempt_at, failures = 0, last_error = NULL",
        params![provider_id, now, now.saturating_add(60)],
    ).map_err(|e| db_err!("failed to clear successful quota refresh error: {e}"))?;
    tx.commit()
        .map_err(|e| db_err!("failed to commit quota snapshot: {e}"))?;
    Ok(())
}

pub(crate) fn read_snapshot(
    conn: &Connection,
    provider_id: i64,
) -> AppResult<Option<OAuthLimitSnapshot>> {
    validate_provider_id(provider_id)?;
    conn.query_row(
        r#"
SELECT
  limit_5h_text,
  limit_weekly_text,
  limit_5h_reset_at,
  limit_weekly_reset_at,
  reset_credit_available_count,
  checked_at,
  short_remaining_percent,
  long_remaining_percent
FROM provider_oauth_limit_snapshots
WHERE provider_id = ?1
"#,
        params![provider_id],
        |row| {
            Ok(OAuthLimitSnapshot {
                short_remaining_percent: row.get(6)?,
                long_remaining_percent: row.get(7)?,
                limit_5h_text: row.get(0)?,
                limit_weekly_text: row.get(1)?,
                limit_5h_reset_at: row.get(2)?,
                limit_weekly_reset_at: row.get(3)?,
                reset_credit_available_count: row.get(4)?,
                checked_at: row.get(5)?,
            })
        },
    )
    .optional()
    .map_err(|e| db_err!("failed to read OAuth limit snapshot: {e}"))
}

pub(crate) fn gate_snapshot(
    conn: &Connection,
    provider_id: i64,
    now_unix: i64,
) -> AppResult<OAuthLimitGate> {
    let Some(snapshot) = read_snapshot(conn, provider_id)? else {
        return Ok(OAuthLimitGate::Allow);
    };

    let mut reset_at = None;
    if let Some(candidate) = active_exhausted_window_reset_at(
        exhausted_text(
            snapshot.short_remaining_percent,
            snapshot.limit_5h_text.as_deref(),
        ),
        snapshot.limit_5h_reset_at,
        snapshot.checked_at,
        now_unix,
    ) {
        update_latest(&mut reset_at, candidate);
    }
    if let Some(candidate) = active_exhausted_window_reset_at(
        exhausted_text(
            snapshot.long_remaining_percent,
            snapshot.limit_weekly_text.as_deref(),
        ),
        snapshot.limit_weekly_reset_at,
        snapshot.checked_at,
        now_unix,
    ) {
        update_latest(&mut reset_at, candidate);
    }

    match reset_at {
        Some(reset_at) => Ok(OAuthLimitGate::Limited {
            reset_at: Some(reset_at),
        }),
        None => Ok(OAuthLimitGate::Allow),
    }
}

fn exhausted_text(percent: Option<f64>, text: Option<&str>) -> Option<&str> {
    match percent {
        Some(value) if value > 0.0 => None,
        Some(_) => Some("0"),
        None => text,
    }
}

pub(crate) fn normalize_percent(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite() && (0.0..=100.0).contains(value))
}

pub(crate) fn gate_with_thresholds(
    conn: &Connection,
    provider_id: i64,
    now: i64,
    short: Option<i64>,
    long: Option<i64>,
) -> AppResult<OAuthLimitGate> {
    let exhausted = gate_snapshot(conn, provider_id, now)?;
    if exhausted != OAuthLimitGate::Allow || (short.is_none() && long.is_none()) {
        return Ok(exhausted);
    }
    let Some(snapshot) = read_snapshot(conn, provider_id)? else {
        return Ok(OAuthLimitGate::Unverified);
    };
    let fresh = snapshot.checked_at <= now
        && now.saturating_sub(snapshot.checked_at) < SNAPSHOT_MAX_AGE_SECS;
    let mut unverified = false;
    let mut hit = false;
    let mut reset = None;
    let mut unknown_reset = false;
    for (threshold, remaining, reset_at) in [
        (
            short,
            snapshot.short_remaining_percent,
            snapshot.limit_5h_reset_at,
        ),
        (
            long,
            snapshot.long_remaining_percent,
            snapshot.limit_weekly_reset_at,
        ),
    ] {
        let Some(threshold) = threshold else { continue };
        if !fresh || reset_at.is_some_and(|reset| reset <= now) {
            unverified = true;
            continue;
        }
        match normalize_percent(remaining) {
            Some(remaining) if remaining <= threshold as f64 => {
                hit = true;
                if let Some(reset_at) = reset_at {
                    update_latest(&mut reset, reset_at);
                } else {
                    unknown_reset = true;
                }
            }
            Some(_) => {}
            None => unverified = true,
        }
    }
    Ok(if hit {
        OAuthLimitGate::Threshold {
            reset_at: if unknown_reset { None } else { reset },
        }
    } else if unverified {
        OAuthLimitGate::Unverified
    } else {
        OAuthLimitGate::Allow
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    #[test]
    fn thresholds_use_raw_precision_and_any_controlled_window() {
        let conn = Connection::open_in_memory().unwrap();
        create_snapshot_table(&conn);
        insert_snapshot(
            &conn,
            7,
            Some("10%"),
            Some("80%"),
            Some(1800),
            Some(3600),
            1000,
        );
        for (short, long, expected) in [
            (10.01, 80.0, OAuthLimitGate::Allow),
            (
                10.0,
                80.0,
                OAuthLimitGate::Threshold {
                    reset_at: Some(1800),
                },
            ),
            (
                11.0,
                5.0,
                OAuthLimitGate::Threshold {
                    reset_at: Some(3600),
                },
            ),
            (
                10.0,
                5.0,
                OAuthLimitGate::Threshold {
                    reset_at: Some(3600),
                },
            ),
        ] {
            conn.execute("UPDATE provider_oauth_limit_snapshots SET short_remaining_percent = ?1, long_remaining_percent = ?2", params![short, long]).unwrap();
            assert_eq!(
                gate_with_thresholds(&conn, 7, 1050, Some(10), Some(5)).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn missing_stale_reset_and_disabled_windows_have_distinct_behavior() {
        let conn = Connection::open_in_memory().unwrap();
        create_snapshot_table(&conn);
        assert_eq!(
            gate_with_thresholds(&conn, 7, 1000, Some(10), None).unwrap(),
            OAuthLimitGate::Unverified
        );
        assert_eq!(
            gate_with_thresholds(&conn, 7, 1000, None, None).unwrap(),
            OAuthLimitGate::Allow
        );
        insert_snapshot(&conn, 7, Some("7 requests"), None, Some(1100), None, 1000);
        assert_eq!(
            gate_with_thresholds(&conn, 7, 1050, Some(10), None).unwrap(),
            OAuthLimitGate::Unverified
        );
        conn.execute(
            "UPDATE provider_oauth_limit_snapshots SET short_remaining_percent = 20",
            [],
        )
        .unwrap();
        assert_eq!(
            gate_with_thresholds(&conn, 7, 1050, Some(10), None).unwrap(),
            OAuthLimitGate::Allow
        );
        assert_eq!(
            gate_with_thresholds(&conn, 7, 1100, Some(10), None).unwrap(),
            OAuthLimitGate::Unverified
        );
        conn.execute(
            "UPDATE provider_oauth_limit_snapshots SET limit_5h_reset_at = NULL",
            [],
        )
        .unwrap();
        assert_eq!(
            gate_with_thresholds(&conn, 7, 1120, Some(10), None).unwrap(),
            OAuthLimitGate::Unverified
        );
        assert_eq!(
            gate_with_thresholds(&conn, 7, 1120, None, None).unwrap(),
            OAuthLimitGate::Allow
        );
    }

    #[test]
    fn precise_positive_fraction_is_not_rounded_to_exhaustion() {
        let conn = Connection::open_in_memory().unwrap();
        create_snapshot_table(&conn);
        insert_snapshot(&conn, 7, Some("0%"), None, Some(1800), None, 1000);
        conn.execute(
            "UPDATE provider_oauth_limit_snapshots SET short_remaining_percent = 0.1",
            [],
        )
        .unwrap();
        assert_eq!(
            gate_with_thresholds(&conn, 7, 1050, Some(0), None).unwrap(),
            OAuthLimitGate::Allow
        );
        assert_eq!(
            gate_with_thresholds(&conn, 7, 1050, Some(1), None).unwrap(),
            OAuthLimitGate::Threshold {
                reset_at: Some(1800)
            }
        );
    }

    #[test]
    fn late_queries_cannot_overwrite_exhaustion_or_new_account() {
        let dir = tempfile::tempdir().unwrap();
        let db = db::init_for_tests(&dir.path().join("late-quota.db")).unwrap();
        let id = insert_test_provider(&db);
        db.open_connection()
            .unwrap()
            .execute(
                "UPDATE providers SET auth_mode = 'oauth' WHERE id = ?1",
                params![id],
            )
            .unwrap();
        let generation = generation(&db, id).unwrap();
        let input = OAuthLimitSnapshotInput {
            provider_id: id,
            short_remaining_percent: Some(90.0),
            long_remaining_percent: None,
            limit_short_label: None,
            limit_5h_text: Some("90%"),
            limit_weekly_text: None,
            limit_5h_reset_at: None,
            limit_weekly_reset_at: None,
            reset_credit_available_count: None,
        };
        db.open_connection().unwrap().execute(
            "INSERT INTO provider_oauth_quota_refresh(provider_id, next_attempt_at, failures, last_error) VALUES (?1, ?2, 2, 'previous failure')",
            params![id, now_unix_seconds() + 600],
        ).unwrap();
        save_snapshot_if_current(&db, input.clone(), generation).unwrap();
        let (failures, error): (i64, Option<String>) = db.open_connection().unwrap().query_row(
            "SELECT failures, last_error FROM provider_oauth_quota_refresh WHERE provider_id = ?1",
            params![id], |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!((failures, error), (0, None));
        save_exhausted_snapshot(&db, id, Some(now_unix_seconds() + 3600)).unwrap();
        assert!(save_snapshot_if_current(&db, input.clone(), generation).is_err());
        assert!(matches!(
            gate_snapshot(&db.open_connection().unwrap(), id, now_unix_seconds()).unwrap(),
            OAuthLimitGate::Limited { .. }
        ));
        let current = super::generation(&db, id).unwrap();
        crate::providers::update_oauth_tokens(
            &db,
            id,
            "oauth",
            "codex_oauth",
            "new_account_token",
            None,
            None,
            "https://auth.example.com/token",
            "client",
            None,
            None,
            None,
        )
        .unwrap();
        assert!(save_snapshot_if_current(&db, input, current).is_err());
        assert!(read_snapshot(&db.open_connection().unwrap(), id)
            .unwrap()
            .is_none());
        let current = super::generation(&db, id).unwrap();
        crate::providers::clear_oauth(&db, id).unwrap();
        assert!(save_snapshot_if_current(
            &db,
            OAuthLimitSnapshotInput {
                provider_id: id,
                short_remaining_percent: Some(90.0),
                long_remaining_percent: None,
                limit_short_label: None,
                limit_5h_text: None,
                limit_weekly_text: None,
                limit_5h_reset_at: None,
                limit_weekly_reset_at: None,
                reset_credit_available_count: None,
            },
            current
        )
        .is_err());
    }

    fn create_snapshot_table(conn: &Connection) {
        conn.execute_batch(
            r#"
CREATE TABLE provider_oauth_limit_snapshots (
  provider_id INTEGER PRIMARY KEY,
  short_remaining_percent REAL,
  long_remaining_percent REAL,
  limit_short_label TEXT,
  limit_5h_text TEXT,
  limit_weekly_text TEXT,
  limit_5h_reset_at INTEGER,
  limit_weekly_reset_at INTEGER,
  reset_credit_available_count INTEGER,
  checked_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
"#,
        )
        .expect("create snapshot table");
    }

    fn insert_snapshot(
        conn: &Connection,
        provider_id: i64,
        limit_5h_text: Option<&str>,
        limit_weekly_text: Option<&str>,
        limit_5h_reset_at: Option<i64>,
        limit_weekly_reset_at: Option<i64>,
        checked_at: i64,
    ) {
        conn.execute(
            r#"
INSERT INTO provider_oauth_limit_snapshots(
  provider_id,
  limit_5h_text,
  limit_weekly_text,
  limit_5h_reset_at,
  limit_weekly_reset_at,
  reset_credit_available_count,
  checked_at,
  updated_at
) VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?6)
"#,
            params![
                provider_id,
                limit_5h_text,
                limit_weekly_text,
                limit_5h_reset_at,
                limit_weekly_reset_at,
                checked_at
            ],
        )
        .expect("insert snapshot");
    }

    fn insert_test_provider(db: &db::Db) -> i64 {
        insert_test_provider_named(db, "OAuth limit snapshot test")
    }

    fn insert_test_provider_named(db: &db::Db, name: &str) -> i64 {
        crate::providers::upsert(
            db,
            crate::providers::ProviderUpsertParams {
                custom_headers: None,
                provider_id: None,
                cli_key: "codex".to_string(),
                name: name.to_string(),
                base_urls: vec!["https://example.test".to_string()],
                base_url_mode: crate::providers::ProviderBaseUrlMode::Order,
                auth_mode: Some(crate::providers::ProviderAuthMode::ApiKey),
                api_key: Some("sk-test".to_string()),
                enabled: true,
                cost_multiplier: 1.0,
                priority: Some(0),
                claude_models: None,
                model_policy: None,
                oauth_short_window_stop_percent: None,
                oauth_long_window_stop_percent: None,
                limit_5h_usd: None,
                limit_daily_usd: None,
                daily_reset_mode: None,
                daily_reset_time: None,
                limit_weekly_usd: None,
                limit_monthly_usd: None,
                limit_total_usd: None,
                tags: None,
                note: None,
                source_provider_id: None,
                bridge_type: None,
                stream_idle_timeout_seconds: None,
                supports_websockets: None,
                extension_values: None,
            },
        )
        .expect("insert provider")
        .id
    }

    #[test]
    fn exhausted_snapshot_limits_until_latest_reset() {
        let conn = Connection::open_in_memory().expect("open");
        create_snapshot_table(&conn);
        insert_snapshot(
            &conn,
            7,
            Some("0%"),
            Some("0"),
            Some(1_800),
            Some(3_600),
            1_000,
        );

        let gate = gate_snapshot(&conn, 7, 1_200).expect("gate");

        assert_eq!(
            gate,
            OAuthLimitGate::Limited {
                reset_at: Some(3_600)
            }
        );
    }

    #[test]
    fn expired_exhausted_snapshot_allows_provider() {
        let conn = Connection::open_in_memory().expect("open");
        create_snapshot_table(&conn);
        insert_snapshot(&conn, 7, Some("0%"), None, Some(1_800), None, 1_000);

        let gate = gate_snapshot(&conn, 7, 1_800).expect("gate");

        assert_eq!(gate, OAuthLimitGate::Allow);
    }

    #[test]
    fn exhausted_snapshot_without_reset_uses_short_fallback_window() {
        let conn = Connection::open_in_memory().expect("open");
        create_snapshot_table(&conn);
        insert_snapshot(&conn, 7, Some("0 requests"), None, None, None, 1_000);

        let gate = gate_snapshot(&conn, 7, 1_100).expect("gate");
        assert_eq!(
            gate,
            OAuthLimitGate::Limited {
                reset_at: Some(1_000 + FALLBACK_COOLDOWN_SECS)
            }
        );

        let expired = gate_snapshot(&conn, 7, 1_000 + FALLBACK_COOLDOWN_SECS).expect("gate");
        assert_eq!(expired, OAuthLimitGate::Allow);
    }

    #[test]
    fn non_zero_snapshot_allows_provider() {
        let conn = Connection::open_in_memory().expect("open");
        create_snapshot_table(&conn);
        insert_snapshot(
            &conn,
            7,
            Some("1%"),
            Some("2"),
            Some(1_800),
            Some(3_600),
            1_000,
        );

        let gate = gate_snapshot(&conn, 7, 1_200).expect("gate");

        assert_eq!(gate, OAuthLimitGate::Allow);
    }

    #[test]
    fn refreshed_available_snapshot_overwrites_exhausted_snapshot() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = db::init_for_tests(&dir.path().join("oauth-limits.db")).expect("init db");
        let now = now_unix_seconds();
        let provider_id = insert_test_provider(&db);

        save_exhausted_snapshot(&db, provider_id, Some(now + 3_600)).expect("save exhausted");
        {
            let conn = db.open_connection().expect("open");
            let gate = gate_snapshot(&conn, provider_id, now).expect("gate");
            assert_eq!(
                gate,
                OAuthLimitGate::Limited {
                    reset_at: Some(now + 3_600)
                }
            );
        }

        save_snapshot(
            &db,
            OAuthLimitSnapshotInput {
                provider_id,
                short_remaining_percent: None,
                long_remaining_percent: None,
                limit_short_label: Some("5h"),
                limit_5h_text: Some("25%"),
                limit_weekly_text: Some("80%"),
                limit_5h_reset_at: None,
                limit_weekly_reset_at: None,
                reset_credit_available_count: Some(2),
            },
        )
        .expect("save refreshed snapshot");

        let conn = db.open_connection().expect("open");
        let gate = gate_snapshot(&conn, provider_id, now).expect("gate");
        assert_eq!(gate, OAuthLimitGate::Allow);
    }

    #[test]
    fn exhausted_snapshot_preserves_existing_future_reset_when_missing_new_reset() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = db::init_for_tests(&dir.path().join("oauth-limits-preserve-reset.db"))
            .expect("init db");
        let now = now_unix_seconds();
        let provider_id = insert_test_provider(&db);

        save_snapshot(
            &db,
            OAuthLimitSnapshotInput {
                provider_id,
                short_remaining_percent: None,
                long_remaining_percent: None,
                limit_short_label: Some("5h"),
                limit_5h_text: Some("1%"),
                limit_weekly_text: Some("10%"),
                limit_5h_reset_at: Some(now + 1_800),
                limit_weekly_reset_at: Some(now + 86_400),
                reset_credit_available_count: Some(5),
            },
        )
        .expect("save current snapshot");

        save_exhausted_snapshot(&db, provider_id, None).expect("save exhausted snapshot");

        let conn = db.open_connection().expect("open");
        let gate = gate_snapshot(&conn, provider_id, now).expect("gate");
        assert_eq!(
            gate,
            OAuthLimitGate::Limited {
                reset_at: Some(now + 86_400)
            }
        );
        let reset_count: Option<i64> = conn
            .query_row(
                "SELECT reset_credit_available_count FROM provider_oauth_limit_snapshots WHERE provider_id = ?1",
                params![provider_id],
                |row| row.get(0),
            )
            .expect("read reset count");
        assert_eq!(reset_count, Some(5));
    }

    #[test]
    fn save_snapshot_persists_reset_credit_available_count_per_provider() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db =
            db::init_for_tests(&dir.path().join("oauth-limits-reset-count.db")).expect("init db");
        let first_provider_id = insert_test_provider_named(&db, "OAuth limit snapshot test 1");
        let second_provider_id = insert_test_provider_named(&db, "OAuth limit snapshot test 2");

        save_snapshot(
            &db,
            OAuthLimitSnapshotInput {
                provider_id: first_provider_id,
                short_remaining_percent: None,
                long_remaining_percent: None,
                limit_short_label: Some("5h"),
                limit_5h_text: Some("25%"),
                limit_weekly_text: Some("80%"),
                limit_5h_reset_at: None,
                limit_weekly_reset_at: None,
                reset_credit_available_count: Some(4),
            },
        )
        .expect("save first snapshot");
        save_snapshot(
            &db,
            OAuthLimitSnapshotInput {
                provider_id: second_provider_id,
                short_remaining_percent: None,
                long_remaining_percent: None,
                limit_short_label: Some("5h"),
                limit_5h_text: Some("90%"),
                limit_weekly_text: Some("95%"),
                limit_5h_reset_at: None,
                limit_weekly_reset_at: None,
                reset_credit_available_count: Some(1),
            },
        )
        .expect("save second snapshot");

        let conn = db.open_connection().expect("open");
        let first_count: Option<i64> = conn
            .query_row(
                "SELECT reset_credit_available_count FROM provider_oauth_limit_snapshots WHERE provider_id = ?1",
                params![first_provider_id],
                |row| row.get(0),
            )
            .expect("read first count");
        let second_count: Option<i64> = conn
            .query_row(
                "SELECT reset_credit_available_count FROM provider_oauth_limit_snapshots WHERE provider_id = ?1",
                params![second_provider_id],
                |row| row.get(0),
            )
            .expect("read second count");

        assert_eq!(first_count, Some(4));
        assert_eq!(second_count, Some(1));
    }

    #[test]
    fn acceptance_oauth_exhausted_snapshot_is_scoped_to_provider() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = db::init_for_tests(&dir.path().join("oauth-limits-scope.db")).expect("init db");
        let now = now_unix_seconds();
        let exhausted_provider_id = insert_test_provider_named(&db, "OAuth exhausted");
        let healthy_provider_id = insert_test_provider_named(&db, "OAuth healthy");

        save_exhausted_snapshot(&db, exhausted_provider_id, Some(now + 3_600))
            .expect("save exhausted snapshot");
        save_snapshot(
            &db,
            OAuthLimitSnapshotInput {
                provider_id: healthy_provider_id,
                short_remaining_percent: None,
                long_remaining_percent: None,
                limit_short_label: Some("5h"),
                limit_5h_text: Some("25%"),
                limit_weekly_text: Some("80%"),
                limit_5h_reset_at: None,
                limit_weekly_reset_at: None,
                reset_credit_available_count: Some(3),
            },
        )
        .expect("save healthy snapshot");

        let conn = db.open_connection().expect("open");
        assert_eq!(
            gate_snapshot(&conn, exhausted_provider_id, now).expect("gate exhausted"),
            OAuthLimitGate::Limited {
                reset_at: Some(now + 3_600)
            }
        );
        assert_eq!(
            gate_snapshot(&conn, healthy_provider_id, now).expect("gate healthy"),
            OAuthLimitGate::Allow
        );
    }
}
