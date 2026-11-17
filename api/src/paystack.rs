//! Paystack: webhook signature check, event parsing, and the two API calls we
//! make (initialise a transaction, verify a transaction).
//!
//! Rules this module enforces, from Paystack's documentation:
//! * A webhook is genuine only if `x-paystack-signature` equals the
//!   HMAC-SHA512 of the **raw request body** keyed with the secret key.
//!   We compare in constant time and never parse before checking.
//! * A webhook alone never grants access. The caller must also call Verify
//!   Transaction and compare status, amount and currency with our own order.

use async_trait::async_trait;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha512;

use crate::error::{AppError, Result};

type HmacSha512 = Hmac<Sha512>;

/// True when `signature_hex` is the HMAC-SHA512 of `body` under `secret`.
pub fn signature_is_valid(secret: &str, body: &[u8], signature_hex: &str) -> bool {
    let Ok(sig) = hex::decode(signature_hex.trim()) else {
        return false;
    };
    let Ok(mut mac) = HmacSha512::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    // verify_slice compares in constant time
    mac.verify_slice(&sig).is_ok()
}

/// Compute the signature Paystack would send. Used by tests and by tooling
/// that replays events.
pub fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac = HmacSha512::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

/// The part of a webhook body we use.
#[derive(Debug, Deserialize)]
pub struct WebhookEvent {
    pub event: String,
    pub data: WebhookData,
}

#[derive(Debug, Deserialize)]
pub struct WebhookData {
    #[serde(default)]
    pub reference: Option<String>,
}

impl WebhookEvent {
    pub fn parse(body: &[u8]) -> Result<Self> {
        serde_json::from_slice(body).map_err(|_| AppError::BadRequest("webhook body is not valid".into()))
    }
}

/// What Paystack's Verify Transaction call tells us. This, not the webhook, is
/// what we trust.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedTransaction {
    /// Paystack's transaction id, used as the idempotency key for payments.
    pub id: String,
    pub reference: String,
    /// `"success"`, `"failed"`, `"abandoned"` ...
    pub status: String,
    pub amount_kobo: i64,
    pub currency: String,
    pub channel: Option<String>,
    pub fees_kobo: i64,
    /// The full gateway answer, kept for audit.
    pub raw: serde_json::Value,
}

/// A transaction we asked Paystack to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitializedTransaction {
    pub authorization_url: String,
    pub reference: String,
}

#[derive(Debug, Clone)]
pub struct InitializeRequest<'a> {
    pub secret: &'a str,
    pub reference: &'a str,
    pub amount_kobo: i64,
    pub currency: &'a str,
    pub email: &'a str,
    pub callback_url: Option<&'a str>,
    /// Paystack subaccount code when the tenant is paid through a subaccount.
    pub subaccount: Option<&'a str>,
    pub metadata: serde_json::Value,
}

/// Everything we need from a payment provider. A trait so tests run without
/// the network.
#[async_trait]
pub trait Gateway: Send + Sync {
    async fn initialize(&self, req: InitializeRequest<'_>) -> Result<InitializedTransaction>;
    async fn verify(&self, secret: &str, reference: &str) -> Result<VerifiedTransaction>;
}

/// The real thing: talks to api.paystack.co.
pub struct PaystackHttp {
    client: reqwest::Client,
    base: String,
}

impl PaystackHttp {
    pub fn new() -> Self {
        Self::with_base("https://api.paystack.co")
    }

    pub fn with_base(base: &str) -> Self {
        let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(20)).build().expect("HTTP client builds");
        PaystackHttp { client, base: base.trim_end_matches('/').to_string() }
    }
}

impl Default for PaystackHttp {
    fn default() -> Self {
        Self::new()
    }
}

fn gw(e: impl std::fmt::Display) -> AppError {
    AppError::Gateway(e.to_string())
}

#[async_trait]
impl Gateway for PaystackHttp {
    async fn initialize(&self, req: InitializeRequest<'_>) -> Result<InitializedTransaction> {
        let mut body = serde_json::json!({
            "email": req.email,
            "amount": req.amount_kobo,
            "currency": req.currency,
            "reference": req.reference,
            "metadata": req.metadata,
        });
        if let Some(cb) = req.callback_url {
            body["callback_url"] = cb.into();
        }
        if let Some(sub) = req.subaccount {
            body["subaccount"] = sub.into();
        }
        let resp = self.client.post(format!("{}/transaction/initialize", self.base)).bearer_auth(req.secret).json(&body).send().await.map_err(gw)?;
        let status = resp.status();
        let v: serde_json::Value = resp.json().await.map_err(gw)?;
        if !status.is_success() || v["status"] != true {
            return Err(gw(format!("initialize rejected ({status}): {}", v["message"])));
        }
        let url = v["data"]["authorization_url"].as_str().ok_or_else(|| gw("no authorization_url"))?;
        Ok(InitializedTransaction { authorization_url: url.to_string(), reference: req.reference.to_string() })
    }

    async fn verify(&self, secret: &str, reference: &str) -> Result<VerifiedTransaction> {
        let resp = self.client.get(format!("{}/transaction/verify/{}", self.base, reference)).bearer_auth(secret).send().await.map_err(gw)?;
        let status = resp.status();
        let v: serde_json::Value = resp.json().await.map_err(gw)?;
        if !status.is_success() || v["status"] != true {
            return Err(gw(format!("verify rejected ({status}): {}", v["message"])));
        }
        parse_verify(&v)
    }
}

/// Turn Paystack's verify answer into our type. Public so tests can feed it samples.
pub fn parse_verify(v: &serde_json::Value) -> Result<VerifiedTransaction> {
    let d = &v["data"];
    // Paystack sends the id as a number; accept a string too.
    let id = match &d["id"] {
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => s.clone(),
        _ => return Err(gw("verify answer has no transaction id")),
    };
    Ok(VerifiedTransaction {
        id,
        reference: d["reference"].as_str().ok_or_else(|| gw("verify answer has no reference"))?.to_string(),
        status: d["status"].as_str().ok_or_else(|| gw("verify answer has no status"))?.to_string(),
        amount_kobo: d["amount"].as_i64().ok_or_else(|| gw("verify answer has no amount"))?,
        currency: d["currency"].as_str().unwrap_or("NGN").to_string(),
        channel: d["channel"].as_str().map(str::to_string),
        fees_kobo: d["fees"].as_i64().unwrap_or(0),
        raw: v.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_correct_signature_passes() {
        let body = br#"{"event":"charge.success","data":{"reference":"ord_X"}}"#;
        let sig = sign("sk_test_secret", body);
        assert!(signature_is_valid("sk_test_secret", body, &sig));
    }

    #[test]
    fn a_wrong_secret_body_or_signature_fails() {
        let body = br#"{"event":"charge.success"}"#;
        let sig = sign("sk_test_secret", body);
        assert!(!signature_is_valid("sk_test_other", body, &sig), "wrong secret");
        assert!(!signature_is_valid("sk_test_secret", br#"{"event":"charge.success "}"#, &sig), "changed body");
        assert!(!signature_is_valid("sk_test_secret", body, "deadbeef"), "short signature");
        assert!(!signature_is_valid("sk_test_secret", body, "not hex at all"), "not hex");
        assert!(!signature_is_valid("sk_test_secret", body, ""), "empty");
    }

    #[test]
    fn signature_check_ignores_hex_case_and_padding_whitespace() {
        let body = b"{}";
        let sig = sign("k", body).to_uppercase();
        assert!(signature_is_valid("k", body, &format!(" {sig} ")));
    }

    #[test]
    fn webhook_event_parses_reference() {
        let e = WebhookEvent::parse(br#"{"event":"charge.success","data":{"reference":"ord_A","amount":100}}"#).unwrap();
        assert_eq!(e.event, "charge.success");
        assert_eq!(e.data.reference.as_deref(), Some("ord_A"));
        assert!(WebhookEvent::parse(b"not json").is_err());
    }

    #[test]
    fn verify_answer_parses_numbers_and_missing_fees() {
        let v = serde_json::json!({"status": true, "data": {
            "id": 4099260516u64, "reference": "ord_A", "status": "success",
            "amount": 50000, "currency": "NGN", "channel": "bank_transfer", "fees": null }});
        let t = parse_verify(&v).unwrap();
        assert_eq!(t.id, "4099260516");
        assert_eq!(t.amount_kobo, 50000);
        assert_eq!(t.fees_kobo, 0);
        assert_eq!(t.channel.as_deref(), Some("bank_transfer"));
        assert!(parse_verify(&serde_json::json!({"data": {}})).is_err());
    }
}
