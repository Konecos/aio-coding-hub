pub(crate) use crate::app::oauth_quota_service::{
    provider_oauth_limits_result_from_parts, ProviderOAuthLimitsResult,
};
use crate::app_state::{ensure_db_ready, DbInitState};

#[tauri::command]
#[specta::specta]
pub(crate) async fn provider_oauth_fetch_limits(
    app: tauri::AppHandle,
    db_state: tauri::State<'_, DbInitState>,
    provider_id: i64,
) -> Result<ProviderOAuthLimitsResult, String> {
    let db = ensure_db_ready(app.clone(), db_state.inner()).await?;
    crate::app::oauth_quota_runtime::refresh(app, db, provider_id).await
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn provider_oauth_quota_states(
    app: tauri::AppHandle,
    db_state: tauri::State<'_, DbInitState>,
) -> Result<Vec<crate::app::oauth_quota_runtime::OAuthQuotaState>, String> {
    let db = ensure_db_ready(app, db_state.inner()).await?;
    crate::blocking::run("oauth_quota_states", move || {
        crate::app::oauth_quota_runtime::states(&db)
    })
    .await
    .map_err(Into::into)
}
