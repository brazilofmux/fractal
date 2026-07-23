//! The Claude API client — raw HTTP against `POST /v1/messages` (there is
//! no official Rust SDK). Credentials come from the environment: an API key
//! (`ANTHROPIC_API_KEY`), a bearer token (`ANTHROPIC_AUTH_TOKEN`), or an
//! `ant auth login` profile via `ant auth print-credentials --access-token`.
//! Bearer tokens are short-lived, so credentials are re-resolved per call.

use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use crate::Writer;

const API_URL: &str = "https://api.anthropic.com/v1/messages";

#[derive(Clone)]
enum Auth {
    ApiKey(String),
    Bearer(String),
}

fn resolve_auth() -> Option<Auth> {
    if let Ok(k) = std::env::var("ANTHROPIC_API_KEY") {
        if !k.trim().is_empty() {
            return Some(Auth::ApiKey(k));
        }
    }
    if let Ok(t) = std::env::var("ANTHROPIC_AUTH_TOKEN") {
        if !t.trim().is_empty() {
            return Some(Auth::Bearer(t));
        }
    }
    // An `ant auth login` profile — the CLI refreshes the token if needed.
    let out = Command::new("ant")
        .args(["auth", "print-credentials", "--access-token"])
        .output()
        .ok()?;
    if out.status.success() {
        let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !token.is_empty() {
            return Some(Auth::Bearer(token));
        }
    }
    None
}

/// The production writer, or None when no credentials can be found (the
/// engine then reports lore as disabled instead of failing per request).
pub(crate) fn api_writer(model: String) -> Option<Writer> {
    resolve_auth()?;
    Some(Arc::new(move |system: &str, user: &str| {
        let body = json!({
            "model": model,
            "max_tokens": 1024,
            "system": [{"type": "text", "text": system}],
            "messages": [{"role": "user", "content": user}],
        });

        let mut last_err = String::new();
        for attempt in 0..3u32 {
            let auth = resolve_auth().ok_or("credentials disappeared")?;
            let req = ureq::post(API_URL)
                .timeout(Duration::from_secs(120))
                .set("anthropic-version", "2023-06-01");
            let req = match &auth {
                Auth::ApiKey(k) => req.set("x-api-key", k),
                Auth::Bearer(t) => req
                    .set("authorization", &format!("Bearer {t}"))
                    .set("anthropic-beta", "oauth-2025-04-20"),
            };
            match req.send_json(body.clone()) {
                Ok(resp) => {
                    let v: serde_json::Value =
                        resp.into_json().map_err(|e| format!("bad response: {e}"))?;
                    return extract_text(&v);
                }
                // Retry what the API says is retryable; report the rest.
                Err(ureq::Error::Status(code, resp)) => {
                    let detail = resp.into_string().unwrap_or_default();
                    last_err = format!("API error {code}: {}", excerpt(&detail));
                    if !(code == 429 || code >= 500) {
                        return Err(last_err);
                    }
                }
                Err(e) => last_err = format!("network error: {e}"),
            }
            std::thread::sleep(Duration::from_secs(2u64.pow(attempt + 1)));
        }
        Err(last_err)
    }))
}

fn extract_text(v: &serde_json::Value) -> Result<String, String> {
    if v["stop_reason"] == "refusal" {
        return Err("the chronicler declined to write this entry".into());
    }
    let text: String = v["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    if text.trim().is_empty() {
        return Err("the chronicler returned an empty page".into());
    }
    Ok(text)
}

fn excerpt(s: &str) -> String {
    let s = s.trim();
    if s.len() > 300 {
        format!("{}…", &s[..300])
    } else {
        s.to_string()
    }
}
