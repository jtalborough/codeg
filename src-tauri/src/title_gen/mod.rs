//! Optional AI-generated short conversation titles.
//!
//! Fully isolated and feature-flag-free: this module compiles unchanged in the
//! desktop (`codeg`), server (`codeg-server`), and MCP companion (`codeg-mcp`)
//! binaries. It has no Tauri dependency and never touches a window.
//!
//! ## What it does
//!
//! At the end of a conversation's first turn, [`maybe_generate_and_apply_title`]
//! asks a configured OpenAI-compatible model provider for a 3–5 word title and
//! writes it via [`conversation_service::refresh_auto_title`] — the
//! lock-respecting, `updated_at`-preserving auto-title path (NOT `update_title`,
//! which would lock the row against the user's own manual rename). On a real
//! write it broadcasts a sidebar upsert so every client sees the new title live.
//!
//! ## Default OFF (zero risk)
//!
//! With the `title_gen.enabled` app-metadata key unset, this code returns early
//! and behavior is identical to upstream. To turn it on, set two
//! `app_metadata` key/value rows:
//!
//! ```text
//! title_gen.enabled     = "true"
//! title_gen.provider_id = "<id of a row in the model_provider table>"
//! ```
//!
//! The referenced provider supplies `api_url`, `api_key`, and `model`.
//!
//! ## Failure model
//!
//! EVERY failure path — no/disabled provider, missing model, HTTP error,
//! timeout, empty or garbage output — resolves to `Ok(None)` (or a silent
//! return). The titler is best-effort and fire-and-forget: it must never panic
//! and never block the turn-completion handler.

use sea_orm::DatabaseConnection;
use serde_json::json;

use crate::db::service::{app_metadata_service, conversation_service, model_provider_service};
use crate::web::event_bridge::EventEmitter;

/// Max characters of user / assistant text fed to the model. Keeps the request
/// small and cheap; a title only needs the gist of the opening exchange.
const MAX_INPUT_CHARS: usize = 600;

/// Hard clamps on the produced title.
const MAX_TITLE_WORDS: usize = 6;
const MAX_TITLE_CHARS: usize = 64;

/// System instruction. Kept terse and imperative so small/cheap models comply.
const SYSTEM_PROMPT: &str =
    "You generate a short title for a coding conversation. \
     Reply with ONLY a 3-5 word title, no quotes, no punctuation.";

/// Boxed error result for [`generate_title`]. In practice every failure path is
/// soft-mapped to `Ok(None)`, so this never carries an `Err` today — it exists
/// to keep the signature a `Result` (per the module contract) without pulling
/// in an `anyhow` dependency the workspace doesn't have.
pub type TitleResult = Result<Option<String>, Box<dyn std::error::Error + Send + Sync>>;

/// Build the user-message content fed to the title model from the opening
/// exchange. Truncates each side and labels them so the model has context
/// without paying for the whole transcript. Pure (no I/O) so it is unit-tested.
fn build_user_content(user_text: &str, assistant_text: &str) -> String {
    let user = truncate_chars(user_text.trim(), MAX_INPUT_CHARS);
    let mut out = format!("User request:\n{user}");
    let assistant = truncate_chars(assistant_text.trim(), MAX_INPUT_CHARS);
    if !assistant.is_empty() {
        out.push_str("\n\nAssistant reply:\n");
        out.push_str(&assistant);
    }
    out
}

/// Truncate to at most `max` chars on a char boundary (not bytes — avoids
/// splitting multi-byte UTF-8). Cheap and lossy by design.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

/// Turn the model's raw reply into a clean title, or `None` if there's nothing
/// usable. Pure so it can be unit-tested without a network:
/// - trim
/// - strip a single layer of surrounding quotes (`"` or `'`)
/// - drop a trailing period
/// - collapse internal whitespace runs to single spaces
/// - clamp to [`MAX_TITLE_WORDS`] words and [`MAX_TITLE_CHARS`] chars
fn sanitize_title(raw: &str) -> Option<String> {
    let mut t = raw.trim().to_string();
    if t.is_empty() {
        return None;
    }
    // Strip one layer of matching surrounding quotes.
    for q in ['"', '\''] {
        if t.len() >= 2 && t.starts_with(q) && t.ends_with(q) {
            t = t[1..t.len() - 1].trim().to_string();
        }
    }
    // Drop a trailing period (models love to add one despite instructions).
    while t.ends_with('.') {
        t.pop();
    }
    // Collapse whitespace runs.
    let collapsed = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    // Clamp word count.
    let mut words: Vec<&str> = collapsed.split(' ').collect();
    if words.len() > MAX_TITLE_WORDS {
        words.truncate(MAX_TITLE_WORDS);
    }
    let mut title = words.join(" ");
    // Clamp char count (defensive; a single very long "word" can blow past).
    if title.chars().count() > MAX_TITLE_CHARS {
        title = truncate_chars(&title, MAX_TITLE_CHARS).trim().to_string();
    }
    if title.is_empty() {
        None
    } else {
        Some(title)
    }
}

/// Parse the `choices[0].message.content` field out of an OpenAI-compatible
/// chat-completion response body, then sanitize it. Returns `None` for any
/// shape that doesn't carry usable content. Pure (takes the raw body string) so
/// the response-handling path is unit-tested without hitting the network.
fn title_from_response_body(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let content = value
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(serde_json::Value::as_str)?;
    sanitize_title(content)
}

/// POST the opening exchange to an OpenAI-compatible `/chat/completions`
/// endpoint and return a sanitized short title, or `Ok(None)` on any
/// soft-failure (HTTP error, timeout, empty/garbage output). Never panics.
///
/// The reqwest client / bearer-auth / timeout idiom mirrors the `/models`
/// validation call in `commands/acp.rs`.
pub async fn generate_title(
    api_url: &str,
    api_key: &str,
    model: &str,
    user_text: &str,
    assistant_text: &str,
) -> TitleResult {
    let base = api_url.trim().trim_end_matches('/');
    let key = api_key.trim();
    let model = model.trim();
    if base.is_empty() || key.is_empty() || model.is_empty() {
        return Ok(None);
    }
    if user_text.trim().is_empty() {
        return Ok(None);
    }

    let url = format!("{base}/chat/completions");
    let payload = json!({
        "model": model,
        "stream": false,
        "max_tokens": 20,
        "temperature": 0.3,
        "messages": [
            { "role": "system", "content": SYSTEM_PROMPT },
            { "role": "user", "content": build_user_content(user_text, assistant_text) },
        ],
    });

    let resp = match reqwest::Client::new()
        .post(&url)
        .bearer_auth(key)
        .json(&payload)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
    {
        Ok(r) => r,
        // Network / timeout / DNS: soft-fail. The titler is optional.
        Err(e) => {
            tracing::debug!("title_gen request failed: {e}");
            return Ok(None);
        }
    };

    if !resp.status().is_success() {
        tracing::debug!("title_gen provider returned {}", resp.status());
        return Ok(None);
    }

    let body = match resp.text().await {
        Ok(b) => b,
        Err(e) => {
            tracing::debug!("title_gen read body failed: {e}");
            return Ok(None);
        }
    };

    Ok(title_from_response_body(&body))
}

/// First-turn entry point: gated by the `title_gen.enabled` flag, resolve the
/// configured provider, generate a title, and apply it via the auto-title path.
/// Fire-and-forget — call from `tokio::spawn`. Any failure is swallowed (logged
/// at debug); it never blocks or fails the caller.
pub async fn maybe_generate_and_apply_title(
    conn: &DatabaseConnection,
    emitter: &EventEmitter,
    conversation_id: i32,
    user_text: String,
    assistant_text: String,
) {
    // Gate 1: feature enabled?
    match app_metadata_service::get_value(conn, "title_gen.enabled").await {
        Ok(Some(v)) if v == "true" => {}
        _ => return,
    }

    // Gate 2: which provider?
    let provider_id = match app_metadata_service::get_value(conn, "title_gen.provider_id").await {
        Ok(Some(v)) => match v.trim().parse::<i32>() {
            Ok(id) => id,
            Err(_) => {
                tracing::debug!("title_gen.provider_id is not an integer: {v:?}");
                return;
            }
        },
        _ => return,
    };

    let provider = match model_provider_service::get_by_id(conn, provider_id).await {
        Ok(Some(p)) => p,
        Ok(None) => {
            tracing::debug!("title_gen.provider_id {provider_id} not found");
            return;
        }
        Err(e) => {
            tracing::debug!("title_gen provider lookup failed: {e}");
            return;
        }
    };

    let model = match provider.model.as_deref() {
        Some(m) if !m.trim().is_empty() => m,
        _ => {
            tracing::debug!("title_gen provider {provider_id} has no model set");
            return;
        }
    };

    let title = match generate_title(
        &provider.api_url,
        &provider.api_key,
        model,
        &user_text,
        &assistant_text,
    )
    .await
    {
        Ok(Some(t)) => t,
        Ok(None) => return,
        Err(e) => {
            tracing::debug!("title_gen generate failed: {e}");
            return;
        }
    };

    // Auto-title path: never locks the row, never bumps updated_at, and is a
    // no-op when the user already renamed (title_locked) or the value is
    // unchanged — so re-running on a later turn is harmless.
    match conversation_service::refresh_auto_title(conn, conversation_id, title).await {
        Ok(true) => {
            crate::commands::conversations::emit_conversation_upsert(
                emitter,
                conn,
                conversation_id,
            )
            .await;
        }
        Ok(false) => {}
        Err(e) => tracing::debug!("title_gen refresh_auto_title failed: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_surrounding_double_quotes() {
        assert_eq!(
            sanitize_title("\"Fix Login Bug\""),
            Some("Fix Login Bug".to_string())
        );
    }

    #[test]
    fn sanitize_strips_surrounding_single_quotes() {
        assert_eq!(
            sanitize_title("'Add Dark Mode'"),
            Some("Add Dark Mode".to_string())
        );
    }

    #[test]
    fn sanitize_drops_trailing_period() {
        assert_eq!(
            sanitize_title("Refactor Auth Module."),
            Some("Refactor Auth Module".to_string())
        );
        // Multiple trailing periods / ellipsis.
        assert_eq!(
            sanitize_title("Setup CI Pipeline..."),
            Some("Setup CI Pipeline".to_string())
        );
    }

    #[test]
    fn sanitize_collapses_whitespace() {
        assert_eq!(
            sanitize_title("  Build   Docker   Image \n"),
            Some("Build Docker Image".to_string())
        );
    }

    #[test]
    fn sanitize_clamps_long_output_to_six_words() {
        let raw = "One Two Three Four Five Six Seven Eight Nine Ten";
        assert_eq!(
            sanitize_title(raw),
            Some("One Two Three Four Five Six".to_string())
        );
    }

    #[test]
    fn sanitize_clamps_long_char_count() {
        // A single 100-char "word" must be clamped to MAX_TITLE_CHARS.
        let raw = "a".repeat(100);
        let out = sanitize_title(&raw).unwrap();
        assert!(out.chars().count() <= MAX_TITLE_CHARS, "got {out:?}");
    }

    #[test]
    fn sanitize_empty_is_none() {
        assert_eq!(sanitize_title(""), None);
        assert_eq!(sanitize_title("   \n\t  "), None);
        // Quotes wrapping nothing.
        assert_eq!(sanitize_title("\"\""), None);
        // Only a period.
        assert_eq!(sanitize_title("."), None);
    }

    #[test]
    fn response_body_extracts_and_sanitizes_content() {
        let body = r#"{
            "choices": [
                { "message": { "role": "assistant", "content": "\"Fix Login Redirect.\"" } }
            ]
        }"#;
        assert_eq!(
            title_from_response_body(body),
            Some("Fix Login Redirect".to_string())
        );
    }

    #[test]
    fn response_body_missing_content_is_none() {
        assert_eq!(title_from_response_body(r#"{"choices": []}"#), None);
        assert_eq!(title_from_response_body(r#"{}"#), None);
        assert_eq!(title_from_response_body("not json at all"), None);
        assert_eq!(
            title_from_response_body(r#"{"choices":[{"message":{"content":""}}]}"#),
            None
        );
    }

    #[test]
    fn build_user_content_includes_both_sides_when_present() {
        let c = build_user_content("Please fix the bug", "Sure, I patched it");
        assert!(c.contains("User request:"));
        assert!(c.contains("Please fix the bug"));
        assert!(c.contains("Assistant reply:"));
        assert!(c.contains("Sure, I patched it"));
    }

    #[test]
    fn build_user_content_omits_assistant_section_when_empty() {
        let c = build_user_content("Just a question", "   ");
        assert!(c.contains("Just a question"));
        assert!(!c.contains("Assistant reply:"));
    }

    #[test]
    fn build_user_content_truncates_each_side() {
        let long = "x".repeat(MAX_INPUT_CHARS + 500);
        let c = build_user_content(&long, &long);
        // Two truncated copies + labels stay well under the raw 2*len.
        assert!(c.chars().count() < (MAX_INPUT_CHARS * 2) + 100);
    }
}
