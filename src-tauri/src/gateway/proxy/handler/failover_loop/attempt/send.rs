//! Usage: Upstream request sending helpers (first-byte timeout aware).

use super::context::CommonCtx;
use axum::body::Bytes;
use axum::http::{HeaderMap, Method};

pub(super) enum SendResult {
    Ok(reqwest::Response),
    Err(reqwest::Error),
    Timeout,
}

pub(super) async fn send_upstream<R: tauri::Runtime>(
    ctx: CommonCtx<'_, R>,
    method: Method,
    url: reqwest::Url,
    headers: HeaderMap,
    body: Bytes,
    provider_id: i64,
    attempt_index: u32,
) -> SendResult {
    if let Some(capture) = crate::gateway::diagnostics::Capture::for_trace(ctx.trace_id) {
        let metadata = crate::gateway::diagnostics::http_metadata(
            &format!(
                "供应商 {} · 尝试 {} · {} {}",
                provider_id,
                attempt_index,
                method,
                crate::gateway::diagnostics::safe_url(&url)
            ),
            &headers,
        );
        let mut capture_body = capture.event("upstream_request", metadata);
        capture_body.chunk(&body);
        capture_body.finish(None);
    }
    let client = ctx.state.client();
    let send = client
        .request(method, url)
        .headers(headers)
        .body(body)
        .send();

    if let Some(timeout) = ctx.upstream_first_byte_timeout {
        match tokio::time::timeout(timeout, send).await {
            Ok(Ok(resp)) => SendResult::Ok(resp),
            Ok(Err(err)) => SendResult::Err(err),
            Err(_) => SendResult::Timeout,
        }
    } else {
        match send.await {
            Ok(resp) => SendResult::Ok(resp),
            Err(err) => SendResult::Err(err),
        }
    }
}
