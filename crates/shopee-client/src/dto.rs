//! Boundary DTOs for Shopee responses.
//!
//! UNSTABLE: Shopee's private JSON is not a contract. Every field here is
//! optional, unknown fields are ignored, and scalar fields accept both the
//! numeric and stringified spelling — a schema drift must degrade a single
//! response to `UNKNOWN_RESPONSE`, never crash a worker.
//!
//! Nothing in this module is re-exported as a domain type: these structs stop
//! at the anti-corruption boundary.

use serde::{Deserialize, Deserializer};

use crate::error::ClientError;

/// The envelope Shopee wraps almost every JSON response in.
///
/// Two spellings have been observed in the wild — `{error, error_msg}` on the
/// older `/api/v2` surface and `{code, msg}` on newer ones — so both are
/// modelled and collapsed by [`ShopeeEnvelope::effective_code`] /
/// [`ShopeeEnvelope::message`].
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ShopeeEnvelope {
    /// UNSTABLE: `/api/v2` style status code. `0` means success.
    #[serde(default, deserialize_with = "de_lenient_i64")]
    pub error: Option<i64>,
    /// UNSTABLE: `/api/v2` style human message, often Vietnamese.
    #[serde(default, deserialize_with = "de_lenient_string")]
    pub error_msg: Option<String>,
    /// UNSTABLE: `/api/v4` style status code. `0` means success.
    #[serde(default, deserialize_with = "de_lenient_i64")]
    pub code: Option<i64>,
    /// UNSTABLE: `/api/v4` style human message.
    #[serde(default, deserialize_with = "de_lenient_string")]
    pub msg: Option<String>,
    /// Endpoint-specific payload, left untyped on purpose.
    #[serde(default)]
    pub data: Option<serde_json::Value>,
}

impl ShopeeEnvelope {
    /// Parse a response body.
    ///
    /// The error detail is structural only (parser category plus line/column):
    /// the body itself is never echoed, because it may be an authenticated
    /// page rather than an API payload.
    pub fn parse(body: &str) -> Result<Self, ClientError> {
        serde_json::from_str(body).map_err(|err| ClientError::MalformedPayload {
            detail: format!(
                "json {:?} at line {} column {}",
                err.classify(),
                err.line(),
                err.column()
            ),
        })
    }

    /// Status code regardless of which spelling the endpoint used.
    pub fn effective_code(&self) -> Option<i64> {
        self.error.or(self.code)
    }

    /// Human message regardless of which spelling the endpoint used.
    pub fn message(&self) -> Option<&str> {
        self.error_msg
            .as_deref()
            .or(self.msg.as_deref())
            .map(str::trim)
            .filter(|m| !m.is_empty())
    }

    /// Whether the envelope carries an explicit success code.
    pub fn is_ok_code(&self) -> bool {
        self.effective_code() == Some(0)
    }

    /// Decode the `data` payload of the account-info probe.
    ///
    /// Returns `None` when `data` is absent or is not an object — both are
    /// treated as "cannot prove the session is healthy".
    pub fn account_info(&self) -> Option<AccountInfoData> {
        let data = self.data.as_ref()?;
        serde_json::from_value::<AccountInfoData>(data.clone()).ok()
    }
}

/// Deliberately minimal view of the account-info payload.
///
/// Only the *presence* of an account identity is modelled. The values stay
/// private and are never exposed, logged, or persisted: proving "someone is
/// logged in" is the entire requirement, and any further field would pull PII
/// into an operational subsystem that has no use for it.
#[derive(Clone, Default, Deserialize)]
pub struct AccountInfoData {
    #[serde(default, deserialize_with = "de_lenient_i64")]
    userid: Option<i64>,
    #[serde(default, deserialize_with = "de_lenient_string")]
    username: Option<String>,
}

impl AccountInfoData {
    /// Whether the payload carries a non-zero account id.
    pub fn has_userid(&self) -> bool {
        matches!(self.userid, Some(id) if id != 0)
    }

    /// Whether the payload carries a non-empty username.
    pub fn has_username(&self) -> bool {
        self.username.as_deref().is_some_and(|u| !u.trim().is_empty())
    }

    /// Whether the payload proves an authenticated identity.
    pub fn identifies_an_account(&self) -> bool {
        self.has_userid() || self.has_username()
    }
}

/// Manual `Debug` so an accidental `?account_info` in a log line can only ever
/// print presence booleans, never the account id or username.
impl std::fmt::Debug for AccountInfoData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountInfoData")
            .field("has_userid", &self.has_userid())
            .field("has_username", &self.has_username())
            .finish()
    }
}

/// Accept `123`, `"123"`, `12.0`, `null` or a missing field as an optional i64.
fn de_lenient_i64<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(serde_json::Value::Number(n)) => n.as_i64().or_else(|| {
            let f = n.as_f64()?;
            if f.is_finite() && f.fract() == 0.0 {
                Some(f as i64)
            } else {
                None
            }
        }),
        Some(serde_json::Value::String(s)) => s.trim().parse::<i64>().ok(),
        _ => None,
    })
}

/// Accept a string, number or bool as an optional string; anything else is
/// dropped rather than failing the whole envelope.
fn de_lenient_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(serde_json::Value::String(s)) => Some(s),
        Some(serde_json::Value::Number(n)) => Some(n.to_string()),
        Some(serde_json::Value::Bool(b)) => Some(b.to_string()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_v2_envelope_and_ignores_unknown_fields() {
        let env = ShopeeEnvelope::parse(
            r#"{"error":0,"error_msg":"","data":{"x":1},"some_new_field":[1,2]}"#,
        )
        .expect("parses");
        assert_eq!(env.effective_code(), Some(0));
        assert!(env.is_ok_code());
        assert_eq!(env.message(), None); // empty message is normalised away
    }

    #[test]
    fn parses_v4_envelope_spelling() {
        let env = ShopeeEnvelope::parse(r#"{"code":7,"msg":"out of stock"}"#).expect("parses");
        assert_eq!(env.effective_code(), Some(7));
        assert_eq!(env.message(), Some("out of stock"));
    }

    #[test]
    fn tolerates_stringified_and_missing_scalars() {
        let env = ShopeeEnvelope::parse(r#"{"error":"5","msg":404}"#).expect("parses");
        assert_eq!(env.effective_code(), Some(5));
        assert_eq!(env.message(), Some("404"));

        let empty = ShopeeEnvelope::parse("{}").expect("parses");
        assert_eq!(empty.effective_code(), None);
        assert_eq!(empty.message(), None);
        assert!(!empty.is_ok_code());

        // Wrong types degrade to None instead of failing the envelope.
        let odd = ShopeeEnvelope::parse(r#"{"error":{"nested":1},"error_msg":["a"]}"#)
            .expect("parses");
        assert_eq!(odd.effective_code(), None);
        assert_eq!(odd.message(), None);
    }

    #[test]
    fn malformed_body_reports_structure_only() {
        let err = ShopeeEnvelope::parse("<html>login</html>").expect_err("must fail");
        let rendered = err.to_string();
        assert!(rendered.contains("line 1"));
        assert!(!rendered.contains("html"), "parser detail echoed the body");
    }

    #[test]
    fn account_info_exposes_presence_only() {
        let env = ShopeeEnvelope::parse(
            r#"{"error":0,"data":{"userid":12345,"username":"buyer","email":"a@b.c"}}"#,
        )
        .expect("parses");
        let info = env.account_info().expect("data decodes");
        assert!(info.has_userid());
        assert!(info.has_username());
        assert!(info.identifies_an_account());

        let debug = format!("{info:?}");
        assert!(!debug.contains("12345"), "debug leaked the account id");
        assert!(!debug.contains("buyer"), "debug leaked the username");
        assert!(debug.contains("has_userid: true"));
    }

    #[test]
    fn account_info_absent_or_empty_proves_nothing() {
        let logged_out = ShopeeEnvelope::parse(r#"{"error":0,"data":null}"#).expect("parses");
        assert!(logged_out.account_info().is_none());

        let blank = ShopeeEnvelope::parse(r#"{"error":0,"data":{"userid":0,"username":" "}}"#)
            .expect("parses");
        let info = blank.account_info().expect("data decodes");
        assert!(!info.identifies_an_account());

        let non_object = ShopeeEnvelope::parse(r#"{"error":0,"data":[1,2,3]}"#).expect("parses");
        assert!(non_object.account_info().is_none());
    }
}
