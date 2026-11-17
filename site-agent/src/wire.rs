//! The JSON the agent exchanges with the central platform.
//! Field names match `api/src/radius.rs`; the tests below use the same
//! literals as the API's integration tests, so a change on either side breaks
//! a test rather than production.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AuthorizeReq {
    pub username: String,
    pub password: String,
    pub mac: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dhcp_hostname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dhcp_vendor_class: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum AuthorizeReply {
    Accept { entitlement_id: String, attributes: Vec<(String, String)> },
    Reject { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Observation {
    pub ttl_lowered_ratio: f32,
    pub distinct_os_families: u32,
    pub connection_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccountingReq {
    /// `start`, `interim` or `stop`.
    pub status: String,
    pub acct_session_id: String,
    pub username: String,
    pub mac: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub session_secs: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminate_cause: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation: Option<Observation>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct AccountingReply {
    /// `continue` or `disconnect`.
    pub action: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub rate_limit: Option<String>,
    #[serde(default)]
    pub session_timeout_secs: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_platforms_accept_and_reject() {
        let a: AuthorizeReply = serde_json::from_str(
            r#"{"result":"accept","entitlement_id":"6f1c","attributes":[["Session-Timeout","3600"],["Mikrotik-Rate-Limit","2000k/5000k"]]}"#,
        )
        .unwrap();
        assert_eq!(
            a,
            AuthorizeReply::Accept {
                entitlement_id: "6f1c".into(),
                attributes: vec![("Session-Timeout".into(), "3600".into()), ("Mikrotik-Rate-Limit".into(), "2000k/5000k".into())]
            }
        );
        let r: AuthorizeReply = serde_json::from_str(r#"{"result":"reject","reason":"device limit reached for this plan"}"#).unwrap();
        assert_eq!(r, AuthorizeReply::Reject { reason: "device limit reached for this plan".into() });
    }

    #[test]
    fn parses_accounting_replies_with_missing_optionals() {
        let c: AccountingReply = serde_json::from_str(r#"{"action":"continue","reason":null,"rate_limit":"2000k/5000k","session_timeout_secs":3540}"#).unwrap();
        assert_eq!((c.action.as_str(), c.session_timeout_secs), ("continue", Some(3540)));
        let d: AccountingReply = serde_json::from_str(r#"{"action":"disconnect","reason":"idle"}"#).unwrap();
        assert_eq!(d.rate_limit, None);
    }

    #[test]
    fn requests_leave_out_empty_optionals() {
        let r = AccountingReq {
            status: "start".into(),
            acct_session_id: "S1".into(),
            username: "tkabc".into(),
            mac: "a4:83:e7:12:34:56".into(),
            ip: None,
            bytes_in: 0,
            bytes_out: 0,
            session_secs: 0,
            ssid: None,
            terminate_cause: None,
            observation: None,
        };
        let v = serde_json::to_value(&r).unwrap();
        assert!(v.get("ip").is_none() && v.get("observation").is_none());
        assert_eq!(v["status"], "start");
    }
}
