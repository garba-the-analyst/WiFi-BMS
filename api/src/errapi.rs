//! The error log's HTTP side: staff pages, the web app's reporter and the site agent's reporter.

use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::admin::{authenticate, csv_field, Perm};
use crate::auth::client_ip;
use crate::db::{audit, tenant_id_by_slug, tenant_tx};
use crate::errlog::{record, NewEvent, SEVERITIES, SOURCES};
use crate::error::{AppError, Result};
use crate::radius::authenticate_node;
use crate::AppState;

#[derive(Debug, Default, Deserialize)]
pub struct Filter {
    pub source: Option<String>,
    pub severity: Option<String>,
    /// `open` (default), `resolved` or `all`.
    pub state: Option<String>,
    pub q: Option<String>,
    pub site: Option<Uuid>,
    pub before: Option<i64>,
    pub limit: Option<i64>,
}

fn check(f: &Filter) -> Result<()> {
    if let Some(s) = f.source.as_deref() {
        if !SOURCES.contains(&s) {
            return Err(AppError::BadRequest("unknown source".into()));
        }
    }
    if let Some(s) = f.severity.as_deref() {
        if !SEVERITIES.contains(&s) {
            return Err(AppError::BadRequest("unknown severity".into()));
        }
    }
    if !matches!(f.state.as_deref(), None | Some("open") | Some("resolved") | Some("all")) {
        return Err(AppError::BadRequest("state must be open, resolved or all".into()));
    }
    Ok(())
}

const LIST_SQL: &str = "
    SELECT e.id, e.ts, e.source, e.severity, e.code, e.message, e.hint, e.fingerprint, e.request_id, e.resolved_at,
           s.name AS site, e.details,
           (SELECT count(*) FROM error_events g WHERE g.fingerprint = e.fingerprint AND g.ts > now() - interval '7 days') AS same_7d
      FROM error_events e LEFT JOIN sites s ON s.id = e.site_id
     WHERE ($1::text IS NULL OR e.source = $1) AND ($2::text IS NULL OR e.severity = $2)
       AND (CASE COALESCE($3, 'open') WHEN 'open' THEN e.resolved_at IS NULL WHEN 'resolved' THEN e.resolved_at IS NOT NULL ELSE true END)
       AND ($4::text IS NULL OR e.message ILIKE $4 OR e.code ILIKE $4 OR COALESCE(e.request_id, '') ILIKE $4)
       AND ($5::uuid IS NULL OR e.site_id = $5) AND ($6::bigint IS NULL OR e.id < $6)
     ORDER BY e.id DESC LIMIT $7";

fn pattern(q: &Option<String>) -> Option<String> {
    q.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(|s| format!("%{}%", s.replace(['%', '_'], "")))
}

pub async fn list(st: &AppState, tenant: &str, h: &HeaderMap, f: Filter) -> Result<Value> {
    let a = authenticate(st, tenant, h, Perm::ErrorsView).await?;
    check(&f)?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let v: Value = sqlx::query_scalar(&format!("SELECT COALESCE(jsonb_agg(t ORDER BY t.id DESC), '[]'::jsonb) FROM ({LIST_SQL}) t"))
        .bind(f.source.as_deref())
        .bind(f.severity.as_deref())
        .bind(f.state.as_deref())
        .bind(pattern(&f.q))
        .bind(f.site)
        .bind(f.before)
        .bind(f.limit.unwrap_or(100).clamp(1, 500))
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(v)
}

/// Counts for the page header: the last 24 hours by source and severity, open critical items,
/// and the problems that repeat most (grouped by fingerprint) over the last 7 days.
pub async fn summary(st: &AppState, tenant: &str, h: &HeaderMap) -> Result<Value> {
    let a = authenticate(st, tenant, h, Perm::ErrorsView).await?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let by_source: Value = sqlx::query_scalar(
        "SELECT COALESCE(jsonb_object_agg(source, n), '{}'::jsonb) FROM (SELECT source, count(*) AS n FROM error_events WHERE ts > now() - interval '24 hours' AND resolved_at IS NULL GROUP BY source) t",
    )
    .fetch_one(&mut *tx)
    .await?;
    let by_severity: Value = sqlx::query_scalar(
        "SELECT COALESCE(jsonb_object_agg(severity, n), '{}'::jsonb) FROM (SELECT severity, count(*) AS n FROM error_events WHERE ts > now() - interval '24 hours' AND resolved_at IS NULL GROUP BY severity) t",
    )
    .fetch_one(&mut *tx)
    .await?;
    let open_critical: i64 =
        sqlx::query_scalar("SELECT count(*) FROM error_events WHERE severity = 'critical' AND resolved_at IS NULL").fetch_one(&mut *tx).await?;
    let top: Value = sqlx::query_scalar(
        "SELECT COALESCE(jsonb_agg(t), '[]'::jsonb) FROM (
           SELECT fingerprint, max(source) AS source, max(severity) AS severity, max(code) AS code, max(message) AS message, count(*) AS times, min(ts) AS first_seen, max(ts) AS last_seen,
                  bool_and(resolved_at IS NOT NULL) AS resolved
             FROM error_events WHERE ts > now() - interval '7 days' GROUP BY fingerprint ORDER BY count(*) DESC LIMIT 8) t",
    )
    .fetch_one(&mut *tx)
    .await?;
    let per_hour: Value = sqlx::query_scalar(
        "SELECT COALESCE(jsonb_agg(jsonb_build_object('hour', h, 'n', n) ORDER BY h), '[]'::jsonb) FROM (
           SELECT date_trunc('hour', ts) AS h, count(*) AS n FROM error_events WHERE ts > now() - interval '24 hours' GROUP BY 1) t",
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(
        json!({ "last_24h_by_source": by_source, "last_24h_by_severity": by_severity, "open_critical": open_critical, "top_problems": top, "per_hour": per_hour }),
    )
}

pub async fn detail(st: &AppState, tenant: &str, h: &HeaderMap, id: i64) -> Result<Value> {
    let a = authenticate(st, tenant, h, Perm::ErrorsView).await?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let v: Option<Value> = sqlx::query_scalar(
        "SELECT to_jsonb(t) FROM (
           SELECT e.id, e.ts, e.source, e.severity, e.code, e.message, e.details, e.hint, e.fingerprint, e.request_id, e.resolved_at,
                  s.name AS site, r.email AS resolved_by,
                  (SELECT count(*) FROM error_events g WHERE g.fingerprint = e.fingerprint) AS same_total,
                  (SELECT min(ts) FROM error_events g WHERE g.fingerprint = e.fingerprint) AS first_seen,
                  (SELECT max(ts) FROM error_events g WHERE g.fingerprint = e.fingerprint) AS last_seen
             FROM error_events e LEFT JOIN sites s ON s.id = e.site_id LEFT JOIN admin_users r ON r.id = e.resolved_by WHERE e.id = $1) t",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    v.ok_or(AppError::NotFound)
}

#[derive(Debug, Deserialize, Default)]
pub struct Resolve {
    /// Also resolve every other event with the same fingerprint.
    #[serde(default)]
    pub all_same: bool,
}

pub async fn resolve(st: &AppState, tenant: &str, h: &HeaderMap, id: i64, req: Resolve) -> Result<Value> {
    let a = authenticate(st, tenant, h, Perm::ErrorsResolve).await?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let fp: String =
        sqlx::query_scalar("SELECT fingerprint FROM error_events WHERE id = $1").bind(id).fetch_optional(&mut *tx).await?.ok_or(AppError::NotFound)?;
    let n = sqlx::query("UPDATE error_events SET resolved_at = now(), resolved_by = $3 WHERE resolved_at IS NULL AND (id = $1 OR ($4 AND fingerprint = $2))")
        .bind(id)
        .bind(&fp)
        .bind(a.id)
        .bind(req.all_same)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    audit(&mut tx, a.tenant, "admin", Some(&a.actor()), "error.resolved", Some(("error", &id.to_string())), json!({ "events": n })).await?;
    tx.commit().await?;
    Ok(json!({ "ok": true, "resolved": n }))
}

/// Everything matching the filter as a spreadsheet, to attach to a support request.
pub async fn csv(st: &AppState, tenant: &str, h: &HeaderMap, f: Filter) -> Result<Response> {
    let a = authenticate(st, tenant, h, Perm::ErrorsView).await?;
    check(&f)?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let rows = sqlx::query(
        "SELECT e.id, e.ts::text AS ts, e.source, e.severity, e.code, e.message, COALESCE(e.hint, '') AS hint, COALESCE(e.request_id, '') AS request_id,
                COALESCE(s.name, '') AS site, e.details::text AS details, COALESCE(e.resolved_at::text, '') AS resolved_at
           FROM error_events e LEFT JOIN sites s ON s.id = e.site_id
          WHERE ($1::text IS NULL OR e.source = $1) AND ($2::text IS NULL OR e.severity = $2)
            AND (CASE COALESCE($3, 'open') WHEN 'open' THEN e.resolved_at IS NULL WHEN 'resolved' THEN e.resolved_at IS NOT NULL ELSE true END)
            AND ($4::text IS NULL OR e.message ILIKE $4 OR e.code ILIKE $4) ORDER BY e.id DESC LIMIT 5000",
    )
    .bind(f.source.as_deref())
    .bind(f.severity.as_deref())
    .bind(f.state.as_deref())
    .bind(pattern(&f.q))
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let mut out = String::from("id,time,source,severity,code,message,hint,request_id,site,details,resolved_at\n");
    for r in &rows {
        let cols: Vec<String> = ["ts", "source", "severity", "code", "message", "hint", "request_id", "site", "details", "resolved_at"]
            .iter()
            .map(|c| csv_field(&r.try_get::<String, _>(*c).unwrap_or_default()))
            .collect();
        out.push_str(&format!("{},{}\n", r.try_get::<i64, _>("id")?, cols.join(",")));
    }
    Ok(([(header::CONTENT_TYPE, "text/csv; charset=utf-8"), (header::CONTENT_DISPOSITION, "attachment; filename=\"errors.csv\"")], out).into_response())
}

// ---------------------------------------------------------------------------
// Reporters
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ClientError {
    pub message: String,
    /// `js` (an uncaught error), `api` (a call that failed) or `render`.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub stack: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub component: Option<String>,
    #[serde(default)]
    pub release: Option<String>,
    #[serde(default)]
    pub extra: Option<Value>,
}

fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Public. The web app reports crashes and failed calls here. Small bodies, rate limited per address,
/// and nothing from the report is trusted beyond being stored as text.
pub async fn client_error(st: &AppState, tenant_slug: &str, headers: &HeaderMap, req: ClientError) -> Result<Value> {
    let ip = client_ip(headers, st.config.trust_proxy).unwrap_or_else(|| "?".into());
    st.limiter.hit(&format!("client-error:{ip}"), 30, std::time::Duration::from_secs(600))?;
    let tenant = tenant_id_by_slug(&st.pool, tenant_slug).await?;
    let kind = req.kind.as_deref().unwrap_or("js");
    let code = if kind == "api" { "webapp.api_error" } else { "webapp.js_error" };
    let severity = if kind == "api" { "warning" } else { "error" };
    let ua = headers.get("user-agent").and_then(|v| v.to_str().ok()).map(|s| clip(s, 200));
    let extra = req.extra.filter(|v| v.to_string().len() < 2000);
    let ev = NewEvent::new("webapp", severity, code, clip(&req.message, 500)).details(json!({
        "kind": kind, "url": req.url.as_deref().map(|u| clip(u, 300)), "component": req.component.as_deref().map(|c| clip(c, 100)),
        "stack": req.stack.as_deref().map(|s| clip(s, 3000)), "release": req.release.as_deref().map(|r| clip(r, 40)),
        "user_agent": ua, "ip": ip, "extra": extra,
    }));
    record(&st.pool, tenant, ev).await;
    Ok(json!({ "ok": true }))
}

#[derive(Debug, Deserialize)]
pub struct NodeEvent {
    pub source: Option<String>,
    pub severity: Option<String>,
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub details: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct NodeErrors {
    pub events: Vec<NodeEvent>,
}

/// The site agent reports hardware and local-service problems (router unreachable, disconnect
/// command failed, offline queue full ...). At most 50 per call.
pub async fn node_errors(st: &AppState, headers: &HeaderMap, req: NodeErrors) -> Result<Value> {
    let n = authenticate_node(st, headers).await?;
    st.limiter.hit(&format!("node-errors:{}", n.token_id), 60, std::time::Duration::from_secs(60))?;
    let mut stored = 0;
    for e in req.events.into_iter().take(50) {
        let source = match e.source.as_deref() {
            Some("radius") => "radius",
            Some("site_agent") => "site_agent",
            _ => "hardware",
        };
        let severity = SEVERITIES.iter().find(|s| Some(**s) == e.severity.as_deref()).copied().unwrap_or("error");
        let code: String = e.code.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '_').take(60).collect();
        if code.is_empty() {
            continue;
        }
        let details = e.details.filter(|v| v.to_string().len() < 4000).unwrap_or_else(|| json!({}));
        let mut ev = NewEvent::new(source, severity, &code, clip(&e.message, 500)).details(details).site(n.site_id);
        ev.node_id = Some(n.token_id);
        record(&st.pool, n.tenant, ev).await;
        stored += 1;
    }
    Ok(json!({ "ok": true, "stored": stored }))
}
