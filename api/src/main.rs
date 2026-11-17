//! Starts the central API.
//!
//! Environment:
//! * `DATABASE_URL`          connect as a login role that is a member of `platform_app`
//! * `PLATFORM_MASTER_KEY`   64 hex characters; protects stored Paystack keys
//! * `PUBLIC_BASE_URL`       e.g. `https://wifi.example.com` (Paystack sends customers back here)
//! * `FALLBACK_EMAIL_DOMAIN` domain for placeholder payer emails (default `guest.invalid`)
//! * `TRUST_PROXY`          `1` when behind Caddy or another proxy you control (default off)
//! * `BIND`                  listen address (default `0.0.0.0:8080`)
//!
//! Subcommand: `create-admin <tenant-slug> <email> <role>` (password in `ADMIN_PASSWORD`).

use std::sync::Arc;

use platform_api::http::router;
use platform_api::paystack::PaystackHttp;
use platform_api::secrets::MasterKey;
use platform_api::{AppState, Config};
use sqlx::postgres::PgPoolOptions;

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("environment variable {name} is required"))
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let pool = PgPoolOptions::new()
        .max_connections(20)
        .connect(&env("DATABASE_URL"))
        .await
        .expect("connect to the database");

    let state = AppState {
        pool,
        gateway: Arc::new(PaystackHttp::new()),
        master_key: MasterKey::from_hex(&env("PLATFORM_MASTER_KEY")).expect("valid PLATFORM_MASTER_KEY"),
        config: Config {
            public_base_url: env("PUBLIC_BASE_URL").trim_end_matches('/').to_string(),
            fallback_email_domain: std::env::var("FALLBACK_EMAIL_DOMAIN").unwrap_or_else(|_| "guest.invalid".into()),
            trust_proxy: std::env::var("TRUST_PROXY").map(|v| v == "1" || v == "true").unwrap_or(false),
        },
        limiter: Arc::new(platform_api::auth::RateLimiter::new()),
        // Replace with your SMS provider's implementation of `SmsSender`.
        sms: Arc::new(platform_api::sms::DisabledSms),
    };

    // `platform-api create-admin <tenant-slug> <email> <role>` with ADMIN_PASSWORD set.
    // Creates the first operator login, then exits. The password stays out of shell history and process lists.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("create-admin") {
        let (Some(slug), Some(email), Some(role)) = (args.get(2), args.get(3), args.get(4)) else {
            eprintln!("usage: ADMIN_PASSWORD=... platform-api create-admin <tenant-slug> <email> <owner|operator|viewer>");
            std::process::exit(2);
        };
        match platform_api::admin::create_admin(&state, slug, email, &env("ADMIN_PASSWORD"), role).await {
            Ok(id) => println!("created admin {id}. Sign in, then enrol 2-step verification."),
            Err(e) => {
                eprintln!("could not create admin: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    let bind = std::env::var("BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let listener = tokio::net::TcpListener::bind(&bind).await.expect("bind address");
    tracing::info!(%bind, "platform-api listening");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("server runs");
}
