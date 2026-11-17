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
}
