//! Centralized secret redaction. Call sites must never hand-roll masking:
//! use these helpers so every log/diagnostic path behaves the same way.

use serde_json::Value;

/// Keys whose values must never appear in logs, diagnostics, or notifications.
const SENSITIVE_KEY_PARTS: &[&str] = &[
    "cookie",
    "authorization",
    "token",
    "secret",
    "password",
    "passwd",
    "csrf",
    "session_id",
    "sessionid",
    "api_key",
    "apikey",
    "spc_", // Shopee session cookie prefix (SPC_EC, SPC_ST, ...)
];

pub const REDACTED: &str = "[REDACTED]";

/// Whether a header/field/config key is secret-bearing.
pub fn is_sensitive_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    SENSITIVE_KEY_PARTS.iter().any(|part| k.contains(part))
}

/// Mask a secret, keeping a short non-reversible prefix for correlation.
pub fn redact_secret(value: &str) -> String {
    if value.len() <= 8 {
        REDACTED.to_string()
    } else {
        format!("{}…{}", &value[..4], REDACTED)
    }
}

/// Redact all sensitive values in a JSON tree, in place.
pub fn redact_json(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                if is_sensitive_key(key) {
                    *v = Value::String(REDACTED.to_string());
                } else {
                    redact_json(v);
                }
            }
        }
        Value::Array(items) => {
            for v in items.iter_mut() {
                redact_json(v);
            }
        }
        _ => {}
    }
}

/// Copy header pairs with sensitive values masked; safe to log.
pub fn redact_headers<'a>(
    headers: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Vec<(String, String)> {
    headers
        .into_iter()
        .map(|(k, v)| {
            if is_sensitive_key(k) {
                (k.to_string(), REDACTED.to_string())
            } else {
                (k.to_string(), v.to_string())
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detects_sensitive_keys_case_insensitively() {
        for key in [
            "Cookie",
            "Set-Cookie",
            "AUTHORIZATION",
            "telegram_bot_token",
            "DB_PASSWORD",
            "x-csrftoken",
            "SPC_EC",
        ] {
            assert!(is_sensitive_key(key), "{key} should be sensitive");
        }
        assert!(!is_sensitive_key("voucher_id"));
        assert!(!is_sensitive_key("promotion_id"));
    }

    #[test]
    fn redaction_audit_covers_every_documented_secret_key() {
        // Every secret-bearing field named in docs/security.md must be masked.
        for key in [
            "cookie",
            "set-cookie",
            "authorization",
            "telegram_bot_token",
            "bot_token",
            "database_password",
            "postgres_password",
            "admin_token",
            "csrf_token",
            "x-csrftoken",
            "session_id",
            "api_key",
            "SPC_EC",
            "SPC_ST",
            "secret_key",
        ] {
            assert!(is_sensitive_key(key), "{key} must be treated as secret");
        }
        // Non-secret domain fields must NOT be masked (avoid over-redaction).
        for key in [
            "voucher_id",
            "promotion_id",
            "source",
            "discount_amount",
            "title",
        ] {
            assert!(!is_sensitive_key(key), "{key} must not be redacted");
        }
    }

    #[test]
    fn redacts_json_recursively() {
        let mut v = json!({
            "voucher": {"code": "FREESHIP", "signature": "abc"},
            "headers": {"cookie": "SPC_EC=verysecretvalue"},
            "list": [{"authorization": "Bearer xyz"}],
        });
        redact_json(&mut v);
        assert_eq!(v["headers"]["cookie"], REDACTED);
        assert_eq!(v["list"][0]["authorization"], REDACTED);
        assert_eq!(v["voucher"]["code"], "FREESHIP");
    }

    #[test]
    fn secret_masking_never_reveals_suffix() {
        assert_eq!(redact_secret("short"), REDACTED);
        let masked = redact_secret("SPC_EC=1234567890abcdef");
        assert!(masked.starts_with("SPC_"));
        assert!(!masked.contains("abcdef"));
    }

    #[test]
    fn header_redaction_masks_only_sensitive_pairs() {
        let out = redact_headers([("cookie", "a=b"), ("user-agent", "ua")]);
        assert_eq!(out[0].1, REDACTED);
        assert_eq!(out[1].1, "ua");
    }
}
