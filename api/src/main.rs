//! Starts the central API.
//!
//! Environment:
//! * `DATABASE_URL`          connect as a login role that is a member of `platform_app`
//! * `PLATFORM_MASTER_KEY`   64 hex characters; protects stored Paystack keys
//! * `PUBLIC_BASE_URL`       e.g. `https://wifi.example.com` (Paystack sends customers back here)
//! * `FALLBACK_EMAIL_DOMAIN` domain for placeholder payer emails (default `guest.invalid`)
//! * `TRUST_PROXY`          `1` when behind Caddy or another proxy you control (default off)
//! * `CORS_ORIGINS`         comma-separated https origins allowed to call the API from a browser
//! * `BIND`                  listen address (default `0.0.0.0:8080`)
//!
//! * `TERMII_API_KEY`, `TERMII_SENDER_ID`  text-message codes (optional)
//! * `SMTP_URL`, `MAIL_FROM`  email codes (optional)
//! * `REQUIRE_2FA`          `0` to switch off the 2-step requirement (development only)
//!
//! Subcommands (they run once and exit):
//! * `create-tenant <slug> <name> <site-slug> <site-name>`  a new establishment
//! * `create-admin <tenant-slug> <email> <role>`           its first staff login (password in `ADMIN_PASSWORD`)

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
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();

    let pool = PgPoolOptions::new().max_connections(20).connect(&env("DATABASE_URL")).await.expect("connect to the database");

    // Text messages: Termii when TERMII_API_KEY and TERMII_SENDER_ID are set, otherwise off.
    let sms: Arc<dyn platform_api::sms::SmsSender> = match (std::env::var("TERMII_API_KEY"), std::env::var("TERMII_SENDER_ID")) {
        (Ok(k), Ok(sender)) if !k.is_empty() => Arc::new(platform_api::sms::TermiiSms::new(&k, &sender)),
        _ => Arc::new(platform_api::sms::DisabledSms),
    };
    // Email: SMTP_URL and MAIL_FROM, otherwise off.
    let mailer: Arc<dyn platform_api::mail::Mailer> = match (std::env::var("SMTP_URL"), std::env::var("MAIL_FROM")) {
        (Ok(url), Ok(from)) if !url.is_empty() => Arc::new(platform_api::mail::SmtpMailer::new(&url, &from).unwrap_or_else(|e| panic!("{e}"))),
        _ => Arc::new(platform_api::mail::DisabledMailer),
    };
    let state = AppState {
        pool,
        gateway: Arc::new(PaystackHttp::new()),
        master_key: MasterKey::from_hex(&env("PLATFORM_MASTER_KEY")).expect("valid PLATFORM_MASTER_KEY"),
        config: Config {
            public_base_url: env("PUBLIC_BASE_URL").trim_end_matches('/').to_string(),
            fallback_email_domain: std::env::var("FALLBACK_EMAIL_DOMAIN").unwrap_or_else(|_| "guest.invalid".into()),
            trust_proxy: std::env::var("TRUST_PROXY").map(|v| v == "1" || v == "true").unwrap_or(false),
            require_2fa: std::env::var("REQUIRE_2FA").map(|v| !(v == "0" || v == "false")).unwrap_or(true),
        },
        limiter: Arc::new(platform_api::auth::RateLimiter::new()),
        // Replace with your SMS provider's implementation of `SmsSender`.
        sms,
        mailer,
    };

    // `platform-api create-admin <tenant-slug> <email> <role>` with ADMIN_PASSWORD set.
    // Creates the first operator login, then exits. The password stays out of shell history and process lists.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("create-admin") {
        let (Some(slug), Some(email), Some(role)) = (args.get(2), args.get(3), args.get(4)) else {
            eprintln!("usage: ADMIN_PASSWORD=... platform-api create-admin <tenant-slug> <email> <system_admin|ceo|admin|customer_rep>");
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

    // `platform-api create-tenant <slug> "<Name>" <site-slug> "<Site name>"`
    if args.get(1).map(String::as_str) == Some("create-tenant") {
        let (Some(slug), Some(name), Some(site), Some(site_name)) = (args.get(2), args.get(3), args.get(4), args.get(5)) else {
            eprintln!("usage: platform-api create-tenant <slug> \"<Name>\" <site-slug> \"<Site name>\"");
            std::process::exit(2);
        };
        match platform_api::tenants::create_tenant(&state.pool, slug, name, site, site_name).await {
            Ok(id) => println!("created {slug} ({id}) with site {site} and the default 2 Mbps speed profile. Next: create-admin {slug} <email> system_admin"),
            Err(e) => {
                eprintln!("could not create tenant: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    // Background: watch for silent site agents and prune old error events.
    tokio::spawn(platform_api::errlog::watch_nodes(state.pool.clone()));

    let bind = std::env::var("BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let listener = tokio::net::TcpListener::bind(&bind).await.expect("bind address");
    tracing::info!(%bind, "platform-api listening");
    let origins = platform_api::harden::parse_origins(&std::env::var("CORS_ORIGINS").unwrap_or_default());
    axum::serve(listener, platform_api::harden::harden(router(state), origins))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("server runs");
}
