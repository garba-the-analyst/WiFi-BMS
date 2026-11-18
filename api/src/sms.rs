//! Sending text messages (access codes, sign-in codes).
//!
//! The platform does not tie you to one SMS provider. Implement [`SmsSender`]
//! for the one you use (Termii, Africa's Talking, Twilio ...) and pass it into
//! `AppState`. Until then `LogSms` writes messages to the log so you can
//! develop locally, and `DisabledSms` refuses to send.

use async_trait::async_trait;

use crate::error::{AppError, Result};

#[async_trait]
pub trait SmsSender: Send + Sync {
    async fn send(&self, phone: &str, text: &str) -> Result<()>;
    /// False for the placeholder that refuses to send.
    fn is_enabled(&self) -> bool {
        true
    }
}

/// Development only: prints the message instead of sending it. It prints
/// sign-in codes in clear text, so never use it in production.
pub struct LogSms;

#[async_trait]
impl SmsSender for LogSms {
    async fn send(&self, phone: &str, text: &str) -> Result<()> {
        tracing::warn!(%phone, %text, "LogSms: message NOT sent (development sender)");
        Ok(())
    }
}

/// Production default until a provider is wired in. Features that need SMS
/// answer "not available" rather than silently pretending.
pub struct DisabledSms;

#[async_trait]
impl SmsSender for DisabledSms {
    async fn send(&self, _phone: &str, _text: &str) -> Result<()> {
        Err(AppError::BadRequest("text messages are not set up for this service yet".into()))
    }
    fn is_enabled(&self) -> bool {
        false
    }
}

/// Development and tests: keeps messages in memory.
#[derive(Default, Clone)]
pub struct MemorySms {
    pub outbox: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>,
}

#[async_trait]
impl SmsSender for MemorySms {
    async fn send(&self, phone: &str, text: &str) -> Result<()> {
        self.outbox.lock().unwrap().push((phone.into(), text.into()));
        Ok(())
    }
}

/// Termii (https://termii.com), a common Nigerian provider.
/// Set `TERMII_API_KEY` and `TERMII_SENDER_ID`. The request follows Termii's documented
/// `POST /api/sms/send`; check it against your account before relying on it, because it
/// was written from the documentation and has not been run against a live account.
pub struct TermiiSms {
    client: reqwest::Client,
    api_key: String,
    sender_id: String,
    base: String,
}

impl TermiiSms {
    pub fn new(api_key: &str, sender_id: &str) -> Self {
        TermiiSms { client: reqwest::Client::new(), api_key: api_key.into(), sender_id: sender_id.into(), base: "https://api.ng.termii.com".into() }
    }
    /// Termii wants the number in international form without a plus sign: 2348031234567.
    pub fn international(phone: &str) -> String {
        let d: String = phone.chars().filter(|c| c.is_ascii_digit()).collect();
        if let Some(rest) = d.strip_prefix('0') {
            format!("234{rest}")
        } else {
            d
        }
    }
}

#[async_trait]
impl SmsSender for TermiiSms {
    async fn send(&self, phone: &str, text: &str) -> Result<()> {
        let body = serde_json::json!({
            "to": Self::international(phone), "from": self.sender_id, "sms": text,
            "type": "plain", "channel": "generic", "api_key": self.api_key,
        });
        let res = self
            .client
            .post(format!("{}/api/sms/send", self.base))
            .json(&body)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("sms: termii unreachable: {e}")))?;
        if !res.status().is_success() {
            let status = res.status();
            let t = res.text().await.unwrap_or_default();
            return Err(AppError::Internal(format!("sms: termii answered {status}: {}", t.chars().take(200).collect::<String>())));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nigerian_numbers_are_made_international() {
        assert_eq!(TermiiSms::international("0803 123 4567"), "2348031234567");
        assert_eq!(TermiiSms::international("+2348031234567"), "2348031234567");
    }
}
