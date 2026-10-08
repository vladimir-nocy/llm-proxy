//! What Anthropic expects from a Claude Code OAuth session: the billing
//! header and Claude Code prefix at the top of the system prompt, and a
//! `metadata.user_id`. Without the billing header, OAuth tokens cannot
//! reach Sonnet and Opus.

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

/// CLI version used for the billing header and User-Agent.
pub const CLI_VERSION: &str = "2.1.109";
/// Salt from Claude Code's `utils/fingerprint.ts`; must match the backend
/// validator.
const FINGERPRINT_SALT: &str = "59cf53e54c78";

/// Derive the `cc_entrypoint` value from an incoming `User-Agent`:
/// `claude-cli`/`claude-code` → `cli`, `vscode`/`Code/` → `vscode`,
/// anything else → `local-agent`, absent → `cli`.
pub fn derive_entrypoint(user_agent: Option<&str>) -> &'static str {
    let Some(ua) = user_agent else {
        return "cli";
    };
    let lower = ua.to_lowercase();
    if lower.contains("claude-cli") || lower.contains("claude-code") {
        "cli"
    } else if lower.contains("vscode") || ua.contains("Code/") {
        "vscode"
    } else {
        "local-agent"
    }
}

/// `SHA256(SALT + msg[4] + msg[7] + msg[20] + version)[..3]`, mirroring
/// Claude Code's fingerprint algorithm.
pub fn fingerprint(message_text: &str, version: &str) -> String {
    let chars: Vec<char> = message_text.chars().collect();
    let extracted: String = [4usize, 7, 20]
        .iter()
        .map(|&i| chars.get(i).copied().unwrap_or('0'))
        .collect();
    let input = format!("{FINGERPRINT_SALT}{extracted}{version}");
    hex(&Sha256::digest(input.as_bytes()))[..3].to_owned()
}

/// The text of the first user message, for fingerprint computation.
fn first_user_message_text(body: &Value) -> String {
    let messages = body.get("messages").and_then(Value::as_array);
    let Some(first_user) = messages.and_then(|m| {
        m.iter()
            .find(|m| m.get("role").and_then(Value::as_str) == Some("user"))
    }) else {
        return String::new();
    };
    match first_user.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .find(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .and_then(|b| b.get("text").and_then(Value::as_str))
            .unwrap_or("")
            .to_owned(),
        _ => String::new(),
    }
}

/// Inject the billing header and Claude Code prefix into `system`, and set
/// `metadata.user_id`. Existing billing/prefix blocks (recognized by
/// content) are preserved in place; client blocks keep their order after
/// them.
pub fn inject(
    body: &mut Value,
    device_id: &str,
    account_uuid: &str,
    session_id: &str,
    entrypoint: &str,
) {
    let blocks = normalize_system(body);
    let billing = billing_block(body, entrypoint);
    let prefix = json!({
        "type": "text",
        "text": "You are Claude Code, Anthropic's official CLI for Claude."
    });
    let mut ordered = Vec::with_capacity(blocks.len() + 2);
    for block in [&billing, &prefix] {
        if !blocks.iter().any(|b| matches_block(b, block)) {
            ordered.push(block.clone());
        }
    }
    ordered.extend(
        blocks
            .into_iter()
            .filter(|b| !matches_block(b, &billing) && !matches_block(b, &prefix)),
    );
    body["system"] = Value::Array(ordered);
    let user_id = json!({
        "device_id": device_id,
        "account_uuid": account_uuid,
        "session_id": session_id,
    });
    if body.get("metadata").and_then(Value::as_object).is_none() {
        body["metadata"] = json!({});
    }
    body["metadata"]["user_id"] = Value::String(user_id.to_string());
}

/// Recognize a previously injected block by its content, so repeated
/// injections do not stack.
fn matches_block(block: &Value, expected: &Value) -> bool {
    let text = block.get("text").and_then(Value::as_str);
    let expected_text = expected.get("text").and_then(Value::as_str);
    match (text, expected_text) {
        (Some(text), Some(expected)) => {
            text.contains(expected)
                || (expected.contains("x-anthropic-billing-header")
                    && text.contains("x-anthropic-billing-header"))
                || (expected.contains("You are Claude Code")
                    && text.contains("You are Claude Code"))
        }
        _ => false,
    }
}

fn billing_block(body: &Value, entrypoint: &str) -> Value {
    let fp = fingerprint(&first_user_message_text(body), CLI_VERSION);
    json!({
        "type": "text",
        "text": format!(
            "x-anthropic-billing-header: cc_version={CLI_VERSION}.{fp}; cc_entrypoint={entrypoint};"
        )
    })
}

fn normalize_system(body: &mut Value) -> Vec<Value> {
    match body.get("system") {
        Some(Value::String(s)) => vec![json!({"type": "text", "text": s})],
        Some(Value::Array(blocks)) => blocks.clone(),
        _ => Vec::new(),
    }
}

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn body() -> Value {
        json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 16,
            "messages": [{"role": "user", "content": "hello world"}]
        })
    }

    #[test]
    fn injects_billing_prefix_and_metadata() {
        let mut body = body();
        inject(&mut body, "device", "account", "session", "cli");
        let system = body["system"].as_array().unwrap();
        assert_eq!(system.len(), 2);
        assert!(
            system[0]["text"]
                .as_str()
                .unwrap()
                .starts_with("x-anthropic-billing-header: cc_version=2.1.109.")
        );
        assert!(
            system[0]["text"]
                .as_str()
                .unwrap()
                .contains("cc_entrypoint=cli;")
        );
        assert_eq!(
            system[1]["text"],
            "You are Code, Anthropic's official CLI for Claude.".replace("Code", "Claude Code")
        );
        let user_id = body["metadata"]["user_id"].as_str().unwrap();
        assert!(user_id.contains("\"device_id\":\"device\""));
        assert!(user_id.contains("\"account_uuid\":\"account\""));
        assert!(user_id.contains("\"session_id\":\"session\""));
    }

    #[test]
    fn preserves_existing_system_blocks() {
        let mut body = json!({
            "messages": [{"role": "user", "content": "hello world"}],
            "system": [{"type": "text", "text": "Be terse."}]
        });
        inject(&mut body, "d", "a", "s", "cli");
        let system = body["system"].as_array().unwrap();
        assert_eq!(system.len(), 3);
        assert_eq!(system[2]["text"], "Be terse.");
    }

    #[test]
    fn converts_string_system_to_array() {
        let mut body = json!({
            "messages": [{"role": "user", "content": "hello world"}],
            "system": "Be terse."
        });
        inject(&mut body, "d", "a", "s", "cli");
        let system = body["system"].as_array().unwrap();
        assert_eq!(system[2]["text"], "Be terse.");
    }

    #[test]
    fn entrypoint_follows_user_agent() {
        assert_eq!(derive_entrypoint(None), "cli");
        assert_eq!(derive_entrypoint(Some("claude-cli/2.1.0")), "cli");
        assert_eq!(derive_entrypoint(Some("Code/1.9 vscode")), "vscode");
        assert_eq!(derive_entrypoint(Some("curl/8.0")), "local-agent");
    }

    #[test]
    fn fingerprint_is_stable_and_three_chars() {
        let a = fingerprint("hello world", CLI_VERSION);
        let b = fingerprint("hello world", CLI_VERSION);
        assert_eq!(a, b);
        assert_eq!(a.len(), 3);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        // Positions 4, 7, and 20 feed the hash; a difference at one of
        // them changes the fingerprint.
        assert_ne!(
            a,
            fingerprint("hellx world", CLI_VERSION),
            "position 4 participates"
        );
    }
}
