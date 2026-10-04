//! Backend quota refresh ownership, scheduling and cached UI decisions.
use super::oauth_quota_service::{fetch_uncached, ProviderOAuthLimitsResult};
use crate::domain::provider_oauth_limits::{self as quota, OAuthLimitGate};
use crate::shared::error::{db_err, AppResult};
use crate::shared::time::now_unix_seconds;
use rusqlite::{params, OptionalExtension};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use tokio::sync::{watch, Semaphore};

pub(crate) const QUOTA_EVENT: &str = "providers:oauth_quota";
type OperationLocks = HashMap<String, Weak<tokio::sync::Mutex<()>>>;
static LOCKS: OnceLock<Mutex<OperationLocks>> = OnceLock::new();
static CONCURRENCY: OnceLock<Semaphore> = OnceLock::new();

pub(crate) async fn acquire_permit() -> Result<tokio::sync::SemaphorePermit<'static>, String> {
    CONCURRENCY
        .get_or_init(|| Semaphore::new(2))
        .acquire()
        .await
        .map_err(|e| e.to_string())
}

pub(crate) fn operation_lock(
    db: &crate::db::Db,
    id: i64,
) -> AppResult<Arc<tokio::sync::Mutex<()>>> {
    let conn = db.open_connection()?;
    let key = format!("{}:{id}", conn.path().unwrap_or_default());
    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks.retain(|_, value| value.strong_count() > 0);
    if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
        return Ok(lock);
    }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    locks.insert(key, Arc::downgrade(&lock));
    Ok(lock)
}

pub(crate) fn notify<R: tauri::Runtime>(app: &tauri::AppHandle<R>, id: i64) {
    super::gateway_runtime_access::app_gateway_clear_recent_errors(app);
    crate::app::heartbeat_watchdog::gated_emit(app, QUOTA_EVENT, id);
}

fn snapshot_result(db: &crate::db::Db, id: i64) -> AppResult<Option<ProviderOAuthLimitsResult>> {
    let conn = db.open_connection()?;
    let Some(snapshot) = quota::read_snapshot(&conn, id)? else {
        return Ok(None);
    };
    let label: Option<String> = conn
        .query_row(
            "SELECT limit_short_label FROM provider_oauth_limit_snapshots WHERE provider_id = ?1",
            params![id],
            |row| row.get(0),
        )
        .map_err(|e| db_err!("failed to read quota label: {e}"))?;
    Ok(Some(ProviderOAuthLimitsResult {
        short_remaining_percent: snapshot.short_remaining_percent,
        long_remaining_percent: snapshot.long_remaining_percent,
        limit_short_label: label,
        limit_5h_text: snapshot.limit_5h_text,
        limit_weekly_text: snapshot.limit_weekly_text,
        limit_5h_reset_at: snapshot.limit_5h_reset_at,
        limit_weekly_reset_at: snapshot.limit_weekly_reset_at,
        reset_credit_available_count: snapshot.reset_credit_available_count,
    }))
}

pub(crate) async fn refresh<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    db: crate::db::Db,
    id: i64,
) -> Result<ProviderOAuthLimitsResult, String> {
    let lock = operation_lock(&db, id).map_err(String::from)?;
    let _guard = match lock.clone().try_lock_owned() {
        Ok(guard) => guard,
        Err(_) => {
            let guard = lock.lock_owned().await;
            let conn = db.open_connection().map_err(String::from)?;
            let error: Option<String> = conn
                .query_row(
                    "SELECT last_error FROM provider_oauth_quota_refresh WHERE provider_id = ?1",
                    params![id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?
                .flatten();
            if let Some(error) = error {
                return Err(error);
            }
            if let Some(result) = snapshot_result(&db, id).map_err(String::from)? {
                return Ok(result);
            }
            guard
        }
    };
    let now = now_unix_seconds();
    {
        let conn = db.open_connection().map_err(String::from)?;
        let blocked: Option<(i64, Option<String>)> = conn.query_row("SELECT next_attempt_at, last_error FROM provider_oauth_quota_refresh WHERE provider_id = ?1 AND failures > 0", params![id], |row| Ok((row.get(0)?, row.get(1)?))).optional().map_err(|e| e.to_string())?;
        if let Some((next, error)) = blocked.filter(|(next, _)| *next > now) {
            return Err(format!(
                "OAuth 额度刷新正在退避，{} 秒后重试：{}",
                next - now,
                error.unwrap_or_default()
            ));
        }
    }
    let _permit = acquire_permit().await?;
    let generation = quota::generation(&db, id).map_err(String::from)?;
    let result = fetch_uncached(app.clone(), db.clone(), id, generation).await;
    let error = result.as_ref().err().cloned();
    crate::blocking::run("quota_refresh_state", move || -> AppResult<()> {
        let conn = db.open_connection()?;
        // Account changes and true exhaustion invalidate older query completions.
        let current = quota::generation(&db, id)?;
        if current != generation { return Ok(()); }
        let now = now_unix_seconds();
        let previous: i64 = conn.query_row("SELECT failures FROM provider_oauth_quota_refresh WHERE provider_id = ?1", params![id], |row| row.get(0)).optional().map_err(|e| db_err!("failed to read quota failures: {e}"))?.unwrap_or(0);
        let failures = if error.is_some() { previous.saturating_add(1) } else { 0 };
        let delay = retry_delay(failures).max(error.as_deref().and_then(retry_after_from_error).unwrap_or(0)) + if failures > 0 { id.rem_euclid(11) } else { 0 };
        conn.execute("INSERT INTO provider_oauth_quota_refresh(provider_id, last_attempt_at, next_attempt_at, failures, last_error) SELECT ?1, ?2, ?3, ?4, ?5 FROM providers WHERE id = ?1 AND oauth_quota_generation = ?6 ON CONFLICT(provider_id) DO UPDATE SET last_attempt_at = excluded.last_attempt_at, next_attempt_at = excluded.next_attempt_at, failures = excluded.failures, last_error = excluded.last_error", params![id, now, now.saturating_add(delay), failures, error, generation]).map_err(|e| db_err!("failed to save quota refresh state: {e}"))?;
        Ok(())
    }).await.map_err(String::from)?;
    notify(&app, id);
    result
}

fn retry_delay(failures: i64) -> i64 {
    match failures {
        0 | 1 => 60,
        2 => 120,
        _ => 300,
    }
}

fn retry_after_from_error(error: &str) -> Option<i64> {
    let suffix = error.split("retry_after_seconds=").nth(1)?;
    suffix
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse::<i64>()
        .ok()
        .filter(|v| *v > 0)
}

#[derive(Debug, Clone, serde::Serialize, specta::Type)]
pub(crate) struct OAuthQuotaState {
    pub provider_id: i64,
    pub protection_enabled: bool,
    pub short_stop_percent: Option<i64>,
    pub long_stop_percent: Option<i64>,
    pub state: String,
    pub checked_at: Option<i64>,
    pub reset_at: Option<i64>,
    pub last_error: Option<String>,
    pub limits: Option<ProviderOAuthLimitsResult>,
}

pub(crate) fn states(db: &crate::db::Db) -> AppResult<Vec<OAuthQuotaState>> {
    let conn = db.open_connection()?;
    let now = now_unix_seconds();
    let mut stmt = conn.prepare("SELECT id, oauth_short_window_stop_percent, oauth_long_window_stop_percent FROM providers WHERE auth_mode = 'oauth'").map_err(|e| db_err!("failed to prepare quota states: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<i64>>(2)?,
            ))
        })
        .map_err(|e| db_err!("failed to read quota states: {e}"))?;
    let mut states = Vec::new();
    for row in rows {
        let (id, short, long) = row.map_err(|e| db_err!("failed to read quota provider: {e}"))?;
        let (state, reset_at) = match quota::gate_with_thresholds(&conn, id, now, short, long)? {
            OAuthLimitGate::Allow => ("available", None),
            OAuthLimitGate::Limited { reset_at } => ("quota_exhausted", reset_at),
            OAuthLimitGate::Threshold { reset_at } => ("threshold_reached", reset_at),
            OAuthLimitGate::Unverified => ("quota_unverified", None),
        };
        let checked_at = quota::read_snapshot(&conn, id)?.map(|snapshot| snapshot.checked_at);
        let last_error = conn
            .query_row(
                "SELECT last_error FROM provider_oauth_quota_refresh WHERE provider_id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| db_err!("failed to read quota refresh error: {e}"))?
            .flatten();
        states.push(OAuthQuotaState {
            provider_id: id,
            protection_enabled: short.is_some() || long.is_some(),
            short_stop_percent: short,
            long_stop_percent: long,
            state: state.into(),
            checked_at,
            reset_at,
            last_error,
            limits: snapshot_result(db, id)?,
        });
    }
    Ok(states)
}

fn due_providers(db: &crate::db::Db) -> AppResult<Vec<i64>> {
    let conn = db.open_connection()?;
    let mut stmt = conn.prepare("SELECT p.id FROM providers p LEFT JOIN provider_oauth_quota_refresh r ON r.provider_id = p.id LEFT JOIN provider_oauth_limit_snapshots s ON s.provider_id = p.id WHERE p.enabled = 1 AND p.auth_mode = 'oauth' AND (p.oauth_short_window_stop_percent IS NOT NULL OR p.oauth_long_window_stop_percent IS NOT NULL) AND (COALESCE(r.next_attempt_at, 0) <= ?1 OR (COALESCE(r.failures, 0) = 0 AND ((p.oauth_short_window_stop_percent IS NOT NULL AND s.limit_5h_reset_at <= ?1 AND s.checked_at < s.limit_5h_reset_at) OR (p.oauth_long_window_stop_percent IS NOT NULL AND s.limit_weekly_reset_at <= ?1 AND s.checked_at < s.limit_weekly_reset_at))))").map_err(|e| db_err!("failed to prepare quota schedule: {e}"))?;
    let rows = stmt
        .query_map(params![now_unix_seconds()], |row| row.get(0))
        .map_err(|e| db_err!("failed to query quota schedule: {e}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| db_err!("failed to read quota schedule: {e}"))
}

pub(crate) fn spawn<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    db: crate::db::Db,
    mut shutdown: watch::Receiver<bool>,
) -> tauri::async_runtime::JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut tasks = tokio::task::JoinSet::new();
        let mut in_flight = HashSet::new();
        loop {
            tokio::select! {
                _ = shutdown.changed() => { tasks.abort_all(); invalidate_pending(&db).await; return; },
                result = tasks.join_next(), if !tasks.is_empty() => {
                    if let Some(Ok(id)) = result { in_flight.remove(&id); }
                    else { in_flight.clear(); }
                    continue;
                },
                _ = interval.tick() => {}
            }
            if *shutdown.borrow() {
                tasks.abort_all();
                invalidate_pending(&db).await;
                return;
            }
            let ids = crate::blocking::run("quota_schedule", {
                let db = db.clone();
                move || due_providers(&db)
            })
            .await;
            let Ok(ids) = ids else { continue };
            for id in ids.into_iter().filter(|id| in_flight.insert(*id)) {
                let app = app.clone();
                let db = db.clone();
                tasks.spawn(async move {
                    let _ = refresh(app, db, id).await;
                    id
                });
            }
        }
    })
}

async fn invalidate_pending(db: &crate::db::Db) {
    let db = db.clone();
    let _ = crate::blocking::run("quota_refresh_shutdown", move || -> AppResult<()> {
        db.open_connection()?.execute("UPDATE providers SET oauth_quota_generation = oauth_quota_generation + 1 WHERE auth_mode = 'oauth' AND (oauth_short_window_stop_percent IS NOT NULL OR oauth_long_window_stop_percent IS NOT NULL)", [])
            .map_err(|e| db_err!("failed to invalidate pending quota refreshes: {e}"))?;
        Ok(())
    }).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refresh_failure_backoff_is_bounded() {
        assert_eq!(retry_delay(0), 60);
        assert_eq!(retry_delay(1), 60);
        assert_eq!(retry_delay(2), 120);
        assert_eq!(retry_delay(30), 300);
        assert_eq!(
            retry_after_from_error("quota status 429; retry_after_seconds=600 - retry later"),
            Some(600)
        );
        assert_eq!(retry_after_from_error("quota status 429"), None);
        assert_eq!(retry_after_from_error("retry_after_seconds=-1"), None);
    }

    #[test]
    fn refresh_locks_are_scoped_to_provider_and_database() {
        let dir = tempfile::tempdir().unwrap();
        let first = crate::db::init_for_tests(&dir.path().join("first.db")).unwrap();
        let second = crate::db::init_for_tests(&dir.path().join("second.db")).unwrap();
        let lock = operation_lock(&first, 1).unwrap();
        assert!(Arc::ptr_eq(
            &lock,
            &operation_lock(&first.clone(), 1).unwrap()
        ));
        assert!(!Arc::ptr_eq(&lock, &operation_lock(&first, 2).unwrap()));
        assert!(!Arc::ptr_eq(&lock, &operation_lock(&second, 1).unwrap()));
    }
}
