//! The error log: every kind of failure, in one place, with enough detail to fix it.
//!
//! Where events come from:
//!
//! | source        | captured by                                                                   |
//! |---------------|-------------------------------------------------------------------------------|
//! | `api`         | [`capture`] middleware: every 5xx answer and every panic                      |
//! | `database`    | the same middleware (a failed query becomes a 500 tagged `database.*`)       |
//! | `payment`     | the same middleware (Paystack unreachable / refused) and [`record`] calls     |
//! | `sms`/`email` | [`record`] calls where a code or message could not be sent                    |
//! | `webapp`      | `POST /v1/t/:tenant/client-errors`, called by the browser app                 |
//! | `hardware`, `site_agent`, `radius` | `POST /v1/node/errors` (the site agent) and the offline-node watcher |
//! | `security`    | [`record`] calls for suspicious events (bad webhook signatures, ...)         |
//!
//! Each event has a `fingerprint` (same problem = same fingerprint, numbers and ids stripped),
//! so the page can show "this happened 37 times" instead of 37 rows. A `hint` tells whoever
//! is troubleshooting what to check first. Writing an event never fails the request that
//! caused it: if the log itself is broken, the problem goes to the process log only.

use std::time::Duration;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::sha256_hex;
use crate::db::{tenant_id_by_slug, tenant_tx};
use crate::error::ErrorDetail;
use crate::AppState;

pub const SOURCES: &[&str] = &["api", "database", "payment", "sms", "email", "webapp", "hardware", "radius", "site_agent", "security", "system"];
pub const SEVERITIES: &[&str] = &["info", "warning", "error", "critical"];

/// How long events are kept.
pub const RETENTION_DAYS: i32 = 90;

#[derive(Debug, Clone)]
pub struct NewEvent {
    pub source: &'static str,
    pub severity: &'static str,
    pub code: String,
    pub message: String,
    pub details: Value,
    pub request_id: Option<String>,
    pub site_id: Option<Uuid>,
    pub node_id: Option<Uuid>,
    pub admin_id: Option<Uuid>,
}

impl NewEvent {
    pub fn new(source: &'static str, severity: &'static str, code: &str, message: impl Into<String>) -> Self {
        NewEvent {
            source,
            severity,
            code: code.to_string(),
            message: message.into(),
            details: json!({}),
            request_id: None,
            site_id: None,
            node_id: None,
            admin_id: None,
        }
    }
    pub fn details(mut self, d: Value) -> Self {
        self.details = d;
        self
    }
    pub fn site(mut self, s: Uuid) -> Self {
        self.site_id = Some(s);
        self
    }
}

/// What to check first, by error code. A missing code simply has no hint.
pub fn hint_for(code: &str) -> Option<&'static str> {
    Some(match code {
        "payment.gateway" => "Paystack could not be reached or refused the request. Check the Paystack secret key in Settings, that the server can reach api.paystack.co, and Paystack's status page.",
        "payment.webhook_signature" => "A request claiming to be Paystack failed the signature check. If real payments are not completing, check the Paystack secret key; otherwise someone is probing the webhook URL (harmless, but note the IP).",
        "database.query" => "A database query failed. Check that PostgreSQL is running and not out of disk space (docker compose ps, docker compose logs db), and that the latest migrations have been applied.",
        "api.internal" => "An unexpected error inside the API. Use the request id to find the full line in the API log (docker compose logs api).",
        "system.panic" => "The API hit a programming error and recovered. Report this with the request id and the details below.",
        "sms.send_failed" => "A text message could not be sent. Check the SMS provider key, sender ID and account balance.",
        "email.send_failed" => "An email could not be sent. Check SMTP_URL and MAIL_FROM, and that the mail provider allows this sender.",
        "webapp.js_error" => "The web app crashed in someone's browser. The stack and page address below show where; check whether it happens on a specific phone or browser.",
        "webapp.api_error" => "The web app could not reach the API or got a server error. If many appear at once, the API or the network is down.",
        "hardware.node_offline" => "The site agent stopped reporting. Check power and internet at the site, then 'systemctl status site-agent' on the site mini-PC.",
        "hardware.node_recovered" => "The site agent is reporting again.",
        "radius.coa_failed" => "A disconnect/limit command to the router failed. Check that the router is reachable from the mini-PC, the CoA secret, and that RouterOS 'incoming' RADIUS is enabled.",
        "hardware.router_unreachable" => "The site agent could not talk to the router. Check the router's power, the cable to the mini-PC, and the REST credentials.",
        "site_agent.central_unreachable" => "The site could not reach the central server. Customers with existing sessions keep working; new logins use the offline queue. Check the site's internet.",
        "site_agent.spool_full" => "The offline queue is full: events are being dropped. Restore the site's internet connection soon.",
        "security.rate_limited" => "Someone hit a rate limit. Repeated entries from one address may be a guessing attempt.",
        _ => return None,
    })
}

/// Group key: same source and code, same message once digits and ids are removed.
pub fn fingerprint(source: &str, code: &str, message: &str) -> String {
    let mut norm = String::with_capacity(message.len());
    let mut last_digit = false;
    for c in message.chars().take(300) {
        if c.is_ascii_digit() {
            if !last_digit {
                norm.push('#');
            }
            last_digit = true;
        } else {
            norm.push(c.to_ascii_lowercase());
            last_digit = false;
        }
    }
    sha256_hex(&format!("{source}|{code}|{norm}"))[..16].to_string()
}

fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Store one event. Best effort: never returns an error.
pub async fn record(pool: &PgPool, tenant: Uuid, ev: NewEvent) {
    let hint = hint_for(&ev.code);
    let fp = fingerprint(ev.source, &ev.code, &ev.message);
    let result = async {
        let mut tx = tenant_tx(pool, tenant).await?;
        sqlx::query(
            "INSERT INTO error_events (tenant_id, source, severity, code, message, details, hint, fingerprint, request_id, site_id, node_id, admin_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        )
        .bind(tenant)
        .bind(ev.source)
        .bind(ev.severity)
        .bind(clip(&ev.code, 80))
        .bind(clip(&ev.message, 1000))
        .bind(&ev.details)
        .bind(hint)
        .bind(&fp)
        .bind(ev.request_id.as_deref())
        .bind(ev.site_id)
        .bind(ev.node_id)
        .bind(ev.admin_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok::<(), crate::error::AppError>(())
    }
    .await;
    if let Err(e) = result {
        tracing::error!(error = %e, code = %ev.code, "could not write to the error log");
    }
}

/// `/v1/t/<slug>/...` -> `<slug>`.
fn tenant_slug_of(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/v1/t/")?;
    rest.split('/').next().filter(|s| !s.is_empty())
}

/// Middleware: record every 5xx answer (and panics) with the request that caused it.
pub async fn capture(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let method = req.method().to_string();
    let path = req.uri().path().to_string();
    let request_id = req.headers().get("x-request-id").and_then(|v| v.to_str().ok()).map(str::to_string);
    let user_agent = req.headers().get("user-agent").and_then(|v| v.to_str().ok()).map(|s| clip(s, 200));
    let res = next.run(req).await;
    let status = res.status();
    if status.is_server_error() && status != axum::http::StatusCode::REQUEST_TIMEOUT {
        let d = res.extensions().get::<ErrorDetail>().cloned();
        let (source, code, message) = match d {
            Some(d) => (d.source, d.code.to_string(), d.message),
            None => ("api", format!("api.http_{}", status.as_u16()), format!("{method} {path} answered {status}")),
        };
        // The slug in the path says which establishment this belongs to. Without one (node calls,
        // health checks) there is no tenant to file it under, so it stays in the process log.
        match tenant_slug_of(&path) {
            Some(slug) => {
                if let Ok(tenant) = tenant_id_by_slug(&st.pool, slug).await {
                    let mut ev = NewEvent::new(source, if source == "database" { "critical" } else { "error" }, &code, message).details(json!({
                        "method": method, "path": path, "status": status.as_u16(), "user_agent": user_agent,
                    }));
                    ev.request_id = request_id;
                    record(&st.pool, tenant, ev).await;
                }
            }
            None => tracing::error!(%path, %status, %code, "server error (no tenant in path)"),
        }
    }
    res
}

/// Turns a panic into a 500 that carries its message for [`capture`].
pub fn on_panic(err: Box<dyn std::any::Any + Send + 'static>) -> Response {
    use axum::response::IntoResponse;
    let msg = if let Some(s) = err.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = err.downcast_ref::<&str>() {
        s.to_string()
    } else {
        "panic with a non-text payload".to_string()
    };
    tracing::error!(panic = %msg, "request handler panicked");
    let mut res = (axum::http::StatusCode::INTERNAL_SERVER_ERROR, axum::Json(json!({ "error": "something went wrong" }))).into_response();
    res.extensions_mut().insert(ErrorDetail { source: "system", code: "system.panic", message: clip(&msg, 500) });
    res
}

/// Background watcher: a site whose agent has been silent for 5 minutes gets ONE
/// `hardware.node_offline` event; when it speaks again, one `hardware.node_recovered`.
pub async fn watch_nodes(pool: PgPool) {
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    loop {
        tick.tick().await;
        if let Err(e) = watch_once(&pool).await {
            tracing::error!(error = %e, "node watcher failed");
        }
    }
}

pub async fn watch_once(pool: &PgPool) -> crate::error::Result<()> {
    let tenants: Vec<Uuid> = sqlx::query_scalar("SELECT all_tenant_ids()").fetch_all(pool).await?;
    for t in tenants {
        let mut tx = tenant_tx(pool, t).await?;
        // Sites whose agent has reported at least once (agent_up) and has now been silent for 5+ minutes
        // with no offline event since its last report.
        let offline: Vec<(Uuid, String, String)> = sqlx::query_as(
            "SELECT s.id, s.name, m.last_seen::text FROM sites s
               JOIN (SELECT site_id, max(ts) AS last_seen FROM metrics WHERE metric = 'agent_up' GROUP BY site_id) m ON m.site_id = s.id
              WHERE m.last_seen < now() - interval '5 minutes'
                AND NOT EXISTS (SELECT 1 FROM error_events e WHERE e.site_id = s.id AND e.code = 'hardware.node_offline' AND e.ts > m.last_seen)",
        )
        .fetch_all(&mut *tx)
        .await?;
        let back: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT s.id, s.name FROM sites s
               JOIN (SELECT site_id, max(ts) AS last_seen FROM metrics WHERE metric = 'agent_up' GROUP BY site_id) m ON m.site_id = s.id
              WHERE m.last_seen >= now() - interval '5 minutes'
                AND EXISTS (SELECT 1 FROM error_events e WHERE e.site_id = s.id AND e.code = 'hardware.node_offline' AND e.ts > now() - interval '30 days'
                              AND e.resolved_at IS NULL)
                AND NOT EXISTS (SELECT 1 FROM error_events r WHERE r.site_id = s.id AND r.code = 'hardware.node_recovered'
                                 AND r.ts > (SELECT max(o.ts) FROM error_events o WHERE o.site_id = s.id AND o.code = 'hardware.node_offline'))",
        )
        .fetch_all(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM error_events WHERE ts < now() - make_interval(days => $1)").bind(RETENTION_DAYS).execute(&mut *tx).await?;
        tx.commit().await?;
        for (site, name, last) in offline {
            record(
                pool,
                t,
                NewEvent::new("hardware", "critical", "hardware.node_offline", format!("Site agent at {name} has stopped reporting"))
                    .site(site)
                    .details(json!({ "site": name, "last_seen": last })),
            )
            .await;
        }
        for (site, name) in back {
            record(
                pool,
                t,
                NewEvent::new("hardware", "info", "hardware.node_recovered", format!("Site agent at {name} is reporting again"))
                    .site(site)
                    .details(json!({ "site": name })),
            )
            .await;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_problem_same_fingerprint() {
        let a = fingerprint("api", "api.internal", "database: connection 4471 refused");
        let b = fingerprint("api", "api.internal", "database: connection 9 refused");
        assert_eq!(a, b);
        assert_ne!(a, fingerprint("api", "api.internal", "database: timeout"));
    }

    #[test]
    fn tenant_comes_from_the_path() {
        assert_eq!(tenant_slug_of("/v1/t/mywifi/orders"), Some("mywifi"));
        assert_eq!(tenant_slug_of("/v1/node/metrics"), None);
        assert_eq!(tenant_slug_of("/healthz"), None);
    }

    #[test]
    fn important_codes_have_hints() {
        for c in ["payment.gateway", "database.query", "hardware.node_offline", "radius.coa_failed", "webapp.js_error"] {
            assert!(hint_for(c).is_some(), "{c}");
        }
        assert!(hint_for("nope").is_none());
    }
}
