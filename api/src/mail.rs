//! Sending email (staff sign-in codes). Same idea as `sms`: a trait, a real
//! sender for production and simple ones for development and tests.
//!
//! Production: set `SMTP_URL` (for example `smtps://user:app-password@smtp.gmail.com:465`)
//! and `MAIL_FROM` (for example `Waste2Light Wi-Fi <no-reply@waste2light.com>`).
//! Without them [`DisabledMailer`] is used and email 2-step verification cannot be enrolled.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

use crate::error::{AppError, Result};

#[async_trait]
pub trait Mailer: Send + Sync {
    async fn send(&self, to: &str, subject: &str, text: &str) -> Result<()>;
    /// False for the placeholder that refuses to send.
    fn is_enabled(&self) -> bool {
        true
    }
}

pub struct DisabledMailer;

#[async_trait]
impl Mailer for DisabledMailer {
    async fn send(&self, _: &str, _: &str, _: &str) -> Result<()> {
        Err(AppError::BadRequest("email is not set up for this service yet".into()))
    }
    fn is_enabled(&self) -> bool {
        false
    }
}

/// Development and tests: keeps messages in memory (tests read the code from here).
#[derive(Default, Clone)]
pub struct MemoryMailer {
    pub outbox: Arc<Mutex<Vec<(String, String, String)>>>,
}

#[async_trait]
impl Mailer for MemoryMailer {
    async fn send(&self, to: &str, subject: &str, text: &str) -> Result<()> {
        tracing::warn!(%to, %subject, "MemoryMailer: message NOT sent (development mailer)");
        self.outbox.lock().unwrap().push((to.into(), subject.into(), text.into()));
        Ok(())
    }
}

pub struct SmtpMailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}

impl SmtpMailer {
    /// `url` like `smtps://user:pass@host:465` or `smtp://user:pass@host:587?tls=required`.
    pub fn new(url: &str, from: &str) -> std::result::Result<Self, String> {
        let from: Mailbox = from.parse().map_err(|e| format!("MAIL_FROM is not a valid address: {e}"))?;
        let transport = AsyncSmtpTransport::<Tokio1Executor>::from_url(url).map_err(|e| format!("SMTP_URL is not valid: {e}"))?.build();
        Ok(SmtpMailer { transport, from })
    }
    #[allow(dead_code)]
    fn with_credentials(host: &str, user: &str, pass: &str, from: &str) -> std::result::Result<Self, String> {
        let from: Mailbox = from.parse().map_err(|e| format!("bad from address: {e}"))?;
        let transport =
            AsyncSmtpTransport::<Tokio1Executor>::relay(host).map_err(|e| e.to_string())?.credentials(Credentials::new(user.into(), pass.into())).build();
        Ok(SmtpMailer { transport, from })
    }
}

#[async_trait]
impl Mailer for SmtpMailer {
    async fn send(&self, to: &str, subject: &str, text: &str) -> Result<()> {
        let to: Mailbox = to.parse().map_err(|_| AppError::BadRequest("that email address is not valid".into()))?;
        let msg =
            Message::builder().from(self.from.clone()).to(to).subject(subject).body(text.to_string()).map_err(|e| AppError::Internal(format!("email: {e}")))?;
        self.transport.send(msg).await.map(|_| ()).map_err(|e| AppError::Internal(format!("email: smtp send failed: {e}")))
    }
}
