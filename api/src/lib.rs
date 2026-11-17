//! Central API for the Starlink Wi-Fi platform.
//!
//! This crate is the money path: a customer picks a plan, pays on Paystack,
//! and a verified payment becomes an entitlement the network can enforce.
//!
//! ```text
//! portal ──POST /orders──▶ create order ──▶ Paystack initialise ──▶ customer pays
//!                                                                        │
//! Paystack ──POST /webhooks/paystack──▶ check signature ──▶ Verify Transaction
//!                                            │
//!                                            ▼
//!                       lock order ─▶ compare amount ─▶ record payment
//!                                  ─▶ grant entitlement ─▶ audit entry
//! ```
//!
//! Every request runs inside a tenant-scoped transaction, so the database's
//! row-level security keeps one operator's customers invisible to another.

pub mod admin;
pub mod auth;
pub mod codes;
pub mod db;
pub mod entitlements_db;
pub mod error;
pub mod fingerprint;
pub mod fulfil;
pub mod harden;
pub mod http;
pub mod member;
pub mod orders;
pub mod paystack;
pub mod radius;
pub mod secrets;
pub mod sms;
pub mod tether;
pub mod totp;

use std::sync::Arc;

use sqlx::{PgConnection, PgPool, Row};
use uuid::Uuid;

use error::{AppError, Result};
use paystack::Gateway;
use secrets::MasterKey;

/// Settings that come from the environment.
#[derive(Debug, Clone)]
pub struct Config {
    /// Where customers return after paying, e.g. `https://wifi.example.com`.
    pub public_base_url: String,
    /// Domain for placeholder payer emails when a customer gives none.
    pub fallback_email_domain: String,
    /// Believe `X-Forwarded-For`. Turn on ONLY when the API sits behind your own reverse proxy.
    pub trust_proxy: bool,
}

/// Shared by every request handler.
#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub gateway: Arc<dyn Gateway>,
    pub master_key: MasterKey,
    pub config: Config,
    pub limiter: Arc<auth::RateLimiter>,
    pub sms: Arc<dyn sms::SmsSender>,
}

impl AppState {
    /// Read and decrypt a tenant's Paystack secret key. Must run inside that
    /// tenant's transaction (row-level security hides everyone else's).
    pub async fn load_paystack_secret(&self, conn: &mut PgConnection, tenant: Uuid) -> Result<String> {
        let row = sqlx::query("SELECT paystack_secret_enc FROM tenant_secrets WHERE tenant_id = $1")
            .bind(tenant)
            .fetch_optional(conn)
            .await?
            .ok_or_else(|| AppError::Internal("tenant has no Paystack key configured".into()))?;
        let enc: String = row.try_get("paystack_secret_enc")?;
        self.master_key.decrypt(&enc)
    }
}
