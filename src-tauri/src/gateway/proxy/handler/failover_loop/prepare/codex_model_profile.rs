//! Per-attempt Responses compatibility; each retry starts from its own semantic body.

use crate::providers::CodexModelProfile;
use axum::body::Bytes;
use serde_json::Value;

pub(super) fn adapt_request(profile: CodexModelProfile, path: &str, body: &Bytes) -> Option<Bytes> {
    if profile != CodexModelProfile::Deepseek
        || !matches!(path.trim_end_matches('/'), "/v1/responses" | "/responses")
    {
        return None;
    }
    let mut value: Value = serde_json::from_slice(body).ok()?;
    let object = value.as_object_mut()?;
    // DeepSeek accepts Responses, but does not store server-side conversations.
    object.insert("store".into(), Value::Bool(false));
    if let Some(effort) = object
        .get_mut("reasoning")
        .and_then(|value| value.get_mut("effort"))
    {
        let normalized = match effort.as_str()? {
            "minimal" => "low",
            "medium" => "high",
            "xhigh" | "ultra" => "max",
            _ => return Some(Bytes::from(value.to_string())),
        };
        *effort = Value::String(normalized.into());
    }
    Some(Bytes::from(value.to_string()))
}

pub(super) fn needs_full_input(profile: CodexModelProfile, body: &Bytes) -> bool {
    profile == CodexModelProfile::Deepseek
        && serde_json::from_slice::<Value>(body)
            .ok()
            .is_some_and(|value| {
                value
                    .get("previous_response_id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| !id.is_empty())
            })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deepseek_preserves_full_tool_history_and_freeform_patch() {
        let value = serde_json::json!({
            "model":"deepseek-flash", "store":true, "reasoning":{"effort":"xhigh"},
            "tools":[{"type":"custom","name":"apply_patch"}],
            "input":[{"role":"user","content":"edit"},
                {"type":"custom_tool_call","call_id":"c","name":"apply_patch","input":"patch"},
                {"type":"custom_tool_call_output","call_id":"c","output":"done"}]
        });
        let body = Bytes::from(value.to_string());
        let adapted: Value = serde_json::from_slice(
            &adapt_request(CodexModelProfile::Deepseek, "/v1/responses", &body).unwrap(),
        )
        .unwrap();
        assert_eq!(adapted["input"], value["input"]);
        assert_eq!(adapted["tools"], value["tools"]);
        assert_eq!(adapted["reasoning"]["effort"], "max");
        assert_eq!(adapted["store"], false);
        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), value);
    }

    #[test]
    fn incremental_input_is_never_silently_stripped() {
        let body = Bytes::from_static(br#"{"previous_response_id":"resp_old","input":[{"type":"function_call_output","call_id":"c","output":"ok"}]}"#);
        assert!(needs_full_input(CodexModelProfile::Deepseek, &body));
        let adapted = adapt_request(CodexModelProfile::Deepseek, "/responses", &body).unwrap();
        assert!(needs_full_input(CodexModelProfile::Deepseek, &adapted));
        assert!(!needs_full_input(
            CodexModelProfile::FunctionCompatible,
            &body
        ));
        assert!(
            adapt_request(CodexModelProfile::FunctionCompatible, "/responses", &body).is_none()
        );
        assert!(adapt_request(CodexModelProfile::Deepseek, "/models", &body).is_none());
    }

    #[test]
    fn reasoning_efforts_are_normalized_without_touching_other_options() {
        for (input, expected) in [
            ("minimal", "low"),
            ("medium", "high"),
            ("xhigh", "max"),
            ("ultra", "max"),
            ("none", "none"),
            ("low", "low"),
            ("high", "high"),
            ("max", "max"),
        ] {
            let body = Bytes::from(
                serde_json::json!({
                    "reasoning":{"effort":input,"summary":"none"}, "stream":true,
                    "input":[{"role":"user","content":"hello"}]
                })
                .to_string(),
            );
            let adapted = adapt_request(CodexModelProfile::Deepseek, "/responses/", &body).unwrap();
            let value: Value = serde_json::from_slice(&adapted).unwrap();
            assert_eq!(value["reasoning"]["effort"], expected);
            assert_eq!(value["reasoning"]["summary"], "none");
            assert_eq!(value["stream"], true);
            assert!(!needs_full_input(CodexModelProfile::Deepseek, &adapted));
            // A failed DeepSeek attempt must not alter the next generic provider's body.
            assert!(
                adapt_request(CodexModelProfile::FunctionCompatible, "/responses", &body).is_none()
            );
            assert_eq!(
                serde_json::from_slice::<Value>(&body).unwrap()["reasoning"]["effort"],
                input
            );
        }
    }
}
