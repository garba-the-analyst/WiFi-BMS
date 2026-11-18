//! Complaints: customers report a problem; customer reps (and everyone above them) work the queue.
//!
//! A customer needs no account. They give a phone number and say what is wrong; if the number belongs
//! to a member the complaint is linked to that account so the rep sees their plans and devices at once.

use axum::http::HeaderMap;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::admin::{authenticate, Perm};
use crate::auth::client_ip;
use crate::db::{audit, tenant_id_by_slug, tenant_tx};
use crate::error::{AppError, Result};
use crate::AppState;

pub const CATEGORIES: &[&str] = &["payment", "connection", "speed", "voucher", "account", "other"];
pub const STATUSES: &[&str] = &["open", "in_progress", "resolved"];

#[derive(Debug, Deserialize)]
pub struct NewComplaint {
    pub phone: String,
    pub message: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub order_ref: Option<String>,
}

/// Public: anyone may report a problem. Rate limited per address and per number.
pub async fn submit(st: &AppState, tenant_slug: &str, headers: &HeaderMap, req: NewComplaint) -> Result<Value> {
    let ip = client_ip(headers, st.config.trust_proxy).unwrap_or_else(|| "?".into());
    st.limiter.hit(&format!("complaint-ip:{ip}"), 8, std::time::Duration::from_secs(3600))?;
    let phone: String = req.phone.chars().filter(|c| c.is_ascii_digit() || *c == '+').collect();
    if !(8..=16).contains(&phone.len()) {
        return Err(AppError::BadRequest("enter a phone number we can call or text you on".into()));
    }
    st.limiter.hit(&format!("complaint-phone:{tenant_slug}:{phone}"), 5, std::time::Duration::from_secs(3600))?;
    let message = req.message.trim();
    if message.chars().count() < 3 || message.chars().count() > 2000 {
        return Err(AppError::BadRequest("describe the problem in a few words (up to 2000 characters)".into()));
    }
    let category = req.category.as_deref().unwrap_or("other");
    if !CATEGORIES.contains(&category) {
        return Err(AppError::BadRequest("unknown category".into()));
    }
    let order_ref = req.order_ref.as_deref().map(str::trim).filter(|r| !r.is_empty()).map(|r| r.chars().take(40).collect::<String>());
    let tenant = tenant_id_by_slug(&st.pool, tenant_slug).await?;
    let mut tx = tenant_tx(&st.pool, tenant).await?;
    // Phone numbers are stored by members in local or international form; match on the last 10 digits.
    let tail: String = phone.chars().filter(char::is_ascii_digit).rev().take(10).collect::<Vec<_>>().into_iter().rev().collect();
    let account: Option<Uuid> = sqlx::query_scalar("SELECT id FROM accounts WHERE regexp_replace(phone, '\\D', '', 'g') LIKE '%' || $1 LIMIT 1")
        .bind(&tail)
        .fetch_optional(&mut *tx)
        .await?;
    let id: Uuid =
        sqlx::query_scalar("INSERT INTO complaints (tenant_id, account_id, phone, order_ref, category, message) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id")
            .bind(tenant)
            .bind(account)
            .bind(&phone)
            .bind(&order_ref)
            .bind(category)
            .bind(message)
            .fetch_one(&mut *tx)
            .await?;
    audit(&mut tx, tenant, "guest", None, "complaint.created", Some(("complaint", &id.to_string())), json!({ "category": category })).await?;
    tx.commit().await?;
    Ok(json!({ "ok": true, "reference": id.simple().to_string()[..8].to_uppercase() }))
}

pub async fn list(st: &AppState, tenant: &str, h: &HeaderMap, status: Option<&str>, q: Option<&str>) -> Result<Value> {
    let a = authenticate(st, tenant, h, Perm::Complaints).await?;
    if let Some(s) = status {
        if !STATUSES.contains(&s) {
            return Err(AppError::BadRequest("unknown status".into()));
        }
    }
    let pat = format!("%{}%", q.unwrap_or("").trim().replace(['%', '_'], ""));
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let v: Value = sqlx::query_scalar(
        "SELECT COALESCE(jsonb_agg(t), '[]'::jsonb) FROM (
           SELECT c.id, upper(left(replace(c.id::text, '-', ''), 8)) AS reference, c.phone, c.category, c.message, c.status, c.order_ref,
                  c.created_at, c.updated_at, c.account_id IS NOT NULL AS is_member, u.email AS assigned_to,
                  (SELECT count(*) FROM complaint_notes n WHERE n.complaint_id = c.id) AS notes
             FROM complaints c LEFT JOIN admin_users u ON u.id = c.assigned_to
            WHERE ($1::text IS NULL OR c.status = $1) AND (c.phone ILIKE $2 OR c.message ILIKE $2 OR COALESCE(c.order_ref, '') ILIKE $2)
            ORDER BY (c.status = 'resolved'), c.created_at DESC LIMIT 200) t",
    )
    .bind(status)
    .bind(pat)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(v)
}

pub async fn detail(st: &AppState, tenant: &str, h: &HeaderMap, id: Uuid) -> Result<Value> {
    let a = authenticate(st, tenant, h, Perm::Complaints).await?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let c = sqlx::query(
        "SELECT c.id, c.phone, c.category, c.message, c.status, c.order_ref, c.created_at::text AS created_text, c.updated_at::text AS updated_text, c.account_id, u.email AS assigned_to
           FROM complaints c LEFT JOIN admin_users u ON u.id = c.assigned_to WHERE c.id = $1",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    let account_id: Option<Uuid> = c.try_get("account_id")?;
    let notes: Value = sqlx::query_scalar(
        "SELECT COALESCE(jsonb_agg(t ORDER BY t.created_at), '[]'::jsonb) FROM (
           SELECT n.id, n.body, n.created_at, u.email AS author, u.display_name FROM complaint_notes n LEFT JOIN admin_users u ON u.id = n.admin_id WHERE n.complaint_id = $1) t",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    let customer: Value = match account_id {
        Some(acct) => sqlx::query_scalar(
            "SELECT jsonb_build_object('id', a.id, 'phone', a.phone, 'name', a.display_name, 'status', a.status::text,
                    'balance_kobo', COALESCE((SELECT sum(amount_kobo) FROM wallet_ledger w WHERE w.account_id = a.id), 0),
                    'live_plans', (SELECT count(*) FROM entitlements e WHERE e.account_id = a.id AND e.status IN ('pending','active')))
               FROM accounts a WHERE a.id = $1",
        )
        .bind(acct)
        .fetch_optional(&mut *tx)
        .await?
        .unwrap_or(Value::Null),
        None => Value::Null,
    };
    let order: Value = match c.try_get::<Option<String>, _>("order_ref")? {
        Some(r) => sqlx::query_scalar(
            "SELECT jsonb_build_object('reference', o.reference, 'status', o.status::text, 'amount_kobo', o.amount_kobo, 'paid_with', o.paid_with, 'created_at', o.created_at)
               FROM orders o WHERE o.reference = $1",
        )
        .bind(r)
        .fetch_optional(&mut *tx)
        .await?
        .unwrap_or(Value::Null),
        None => Value::Null,
    };
    tx.commit().await?;
    Ok(json!({
        "id": c.try_get::<Uuid, _>("id")?, "phone": c.try_get::<String, _>("phone")?, "category": c.try_get::<String, _>("category")?,
        "message": c.try_get::<String, _>("message")?, "status": c.try_get::<String, _>("status")?, "order_ref": c.try_get::<Option<String>, _>("order_ref")?,
        "assigned_to": c.try_get::<Option<String>, _>("assigned_to")?,
        "created_at": c.try_get::<String, _>("created_text")?, "updated_at": c.try_get::<String, _>("updated_text")?,
        "notes": notes, "customer": customer, "order": order,
    }))
}

#[derive(Debug, Deserialize)]
pub struct Note {
    pub body: String,
}

pub async fn add_note(st: &AppState, tenant: &str, h: &HeaderMap, id: Uuid, req: Note) -> Result<Value> {
    let a = authenticate(st, tenant, h, Perm::Complaints).await?;
    let body = req.body.trim();
    if body.is_empty() || body.chars().count() > 2000 {
        return Err(AppError::BadRequest("write a note of up to 2000 characters".into()));
    }
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM complaints WHERE id = $1 FOR UPDATE").bind(id).fetch_optional(&mut *tx).await?.ok_or(AppError::NotFound)?;
    sqlx::query("INSERT INTO complaint_notes (tenant_id, complaint_id, admin_id, body) VALUES ($1, $2, $3, $4)")
        .bind(a.tenant)
        .bind(id)
        .bind(a.id)
        .bind(body)
        .execute(&mut *tx)
        .await?;
    // A first reply moves an open complaint on.
    sqlx::query("UPDATE complaints SET updated_at = now(), status = CASE WHEN status = 'open' THEN 'in_progress' ELSE status END WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    audit(&mut tx, a.tenant, "admin", Some(&a.actor()), "complaint.note", Some(("complaint", &id.to_string())), json!({})).await?;
    tx.commit().await?;
    Ok(json!({ "ok": true }))
}

#[derive(Debug, Deserialize)]
pub struct Update {
    #[serde(default)]
    pub status: Option<String>,
    /// `me`, or `none` to unassign.
    #[serde(default)]
    pub assign: Option<String>,
}

pub async fn update(st: &AppState, tenant: &str, h: &HeaderMap, id: Uuid, req: Update) -> Result<Value> {
    let a = authenticate(st, tenant, h, Perm::Complaints).await?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM complaints WHERE id = $1 FOR UPDATE").bind(id).fetch_optional(&mut *tx).await?.ok_or(AppError::NotFound)?;
    if let Some(s) = req.status.as_deref() {
        if !STATUSES.contains(&s) {
            return Err(AppError::BadRequest("unknown status".into()));
        }
        sqlx::query("UPDATE complaints SET status = $2, updated_at = now(), resolved_at = CASE WHEN $2 = 'resolved' THEN now() ELSE NULL END WHERE id = $1")
            .bind(id)
            .bind(s)
            .execute(&mut *tx)
            .await?;
    }
    match req.assign.as_deref() {
        Some("me") => {
            sqlx::query("UPDATE complaints SET assigned_to = $2, updated_at = now(), status = CASE WHEN status = 'open' THEN 'in_progress' ELSE status END WHERE id = $1").bind(id).bind(a.id).execute(&mut *tx).await?;
        }
        Some("none") => {
            sqlx::query("UPDATE complaints SET assigned_to = NULL, updated_at = now() WHERE id = $1").bind(id).execute(&mut *tx).await?;
        }
        Some(_) => return Err(AppError::BadRequest("assign must be 'me' or 'none'".into())),
        None => {}
    }
    audit(
        &mut tx,
        a.tenant,
        "admin",
        Some(&a.actor()),
        "complaint.updated",
        Some(("complaint", &id.to_string())),
        json!({ "status": req.status, "assign": req.assign }),
    )
    .await?;
    tx.commit().await?;
    Ok(json!({ "ok": true }))
}
