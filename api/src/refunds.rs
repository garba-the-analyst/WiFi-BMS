//! Refunds: customers ask, an owner decides.
//!
//! * A customer (guest or member) can ask for a refund with the order
//!   reference they were given at checkout. That only records a request.
//! * An **owner** executes a refund. Money moves in one database transaction
//!   that holds the order's row lock, so two clicks cannot refund twice (and a
//!   UNIQUE index backstops it):
//!     - paid by Paystack: Paystack is asked to return the money to the
//!       customer's original method. If Paystack refuses, everything rolls back.
//!     - paid from the wallet: the wallet is credited.
//!     - a wallet top-up: the wallet is debited first (only possible while the
//!       member still has that money) and then Paystack refunds it.
//! * Any plan the order produced is cancelled; a live session ends at the
//!   router's next usage update.
//! * Orders that Paystack charged the wrong amount for (`refund_flagged`) are
//!   refunded for what was actually paid, from the verified payment record.

use axum::http::HeaderMap;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::admin::{authenticate, Perm};
use crate::db::{audit, tenant_id_by_slug, tenant_tx};
use crate::error::{AppError, Result};
use crate::AppState;

const REQUEST_WINDOW_DAYS: i32 = 14;
const REFUNDABLE: [&str; 3] = ["paid", "provisioned", "refund_flagged"];

#[derive(Debug, Deserialize)]
pub struct RefundAsk {
    #[serde(default)]
    pub reason: Option<String>,
}

/// Customer: ask for a refund. The unguessable order reference is the proof of purchase.
pub async fn request(st: &AppState, tenant_slug: &str, headers: &HeaderMap, reference: &str, ask: RefundAsk) -> Result<Value> {
    let ip = crate::auth::client_ip(headers, st.config.trust_proxy).unwrap_or_else(|| "?".into());
    st.limiter.hit(&format!("refund-ip:{ip}"), 10, std::time::Duration::from_secs(3600))?;
    let reason: Option<String> = ask.reason.map(|r| r.trim().chars().take(500).collect()).filter(|r: &String| !r.is_empty());
    let tenant = tenant_id_by_slug(&st.pool, tenant_slug).await?;
    let mut tx = tenant_tx(&st.pool, tenant).await?;
    let o = sqlx::query(
        "SELECT id, status::text AS status, amount_kobo, (created_at > now() - make_interval(days => $2)) AS recent FROM orders WHERE reference = $1 FOR UPDATE",
    )
    .bind(reference)
    .bind(REQUEST_WINDOW_DAYS)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    let (id, status, amount): (Uuid, String, i64) = (o.try_get("id")?, o.try_get("status")?, o.try_get("amount_kobo")?);
    if status == "refunded" {
        return Err(AppError::BadRequest("this order has already been refunded".into()));
    }
    if !REFUNDABLE.contains(&status.as_str()) {
        return Err(AppError::BadRequest("only paid orders can be refunded".into()));
    }
    if !o.try_get::<bool, _>("recent")? {
        return Err(AppError::BadRequest(format!("refund requests must be made within {REQUEST_WINDOW_DAYS} days of the purchase")));
    }
    let made = sqlx::query(
        "INSERT INTO refunds (tenant_id, order_id, amount_kobo, status, requested_by, reason) VALUES ($1, $2, $3, 'requested', 'customer', $4)
         ON CONFLICT (order_id) WHERE status IN ('requested', 'processed') DO NOTHING",
    )
    .bind(tenant)
    .bind(id)
    .bind(amount)
    .bind(reason)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if made == 0 {
        return Err(AppError::BadRequest("a refund has already been requested for this order".into()));
    }
    audit(&mut tx, tenant, "guest", None, "refund.requested", Some(("order", &id.to_string())), json!({ "amount_kobo": amount })).await?;
    tx.commit().await?;
    Ok(json!({ "status": "requested" }))
}

/// Customer: where is my request? Uses the same order reference.
pub async fn request_status(st: &AppState, tenant_slug: &str, reference: &str) -> Result<Value> {
    let tenant = tenant_id_by_slug(&st.pool, tenant_slug).await?;
    let mut tx = tenant_tx(&st.pool, tenant).await?;
    let status: Option<String> =
        sqlx::query_scalar("SELECT r.status FROM refunds r JOIN orders o ON o.id = r.order_id WHERE o.reference = $1 ORDER BY r.created_at DESC LIMIT 1")
            .bind(reference)
            .fetch_optional(&mut *tx)
            .await?;
    tx.commit().await?;
    Ok(json!({ "status": status.unwrap_or_else(|| "none".into()) }))
}

/// Admin: orders, newest first, to find what to refund.
pub async fn list_orders(
    st: &AppState,
    tenant: &str,
    h: &HeaderMap,
    q: Option<&str>,
    status: Option<&str>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Value> {
    let a = authenticate(st, tenant, h, Perm::Orders).await?;
    if let Some(s) = status {
        if !["created", "pending", "paid", "provisioned", "failed", "expired", "refund_flagged", "refunded"].contains(&s) {
            return Err(AppError::BadRequest("unknown status".into()));
        }
    }
    let pat = format!("%{}%", q.unwrap_or("").trim().replace(['%', '_'], ""));
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let v: Value = sqlx::query_scalar(
        "SELECT COALESCE(jsonb_agg(t), '[]'::jsonb) FROM (
           SELECT o.id, o.reference, o.status::text AS status, o.purpose, o.paid_with, o.amount_kobo, o.phone, o.created_at, o.paid_at,
                  COALESCE(p.name, 'Wallet top-up') AS plan,
                  (SELECT r.status FROM refunds r WHERE r.order_id = o.id ORDER BY r.created_at DESC LIMIT 1) AS refund_status
             FROM orders o LEFT JOIN plans p ON p.id = o.plan_id
            WHERE ($1::text IS NULL OR o.status::text = $1) AND (o.reference ILIKE $2 OR COALESCE(o.phone, '') ILIKE $2)
            ORDER BY o.created_at DESC LIMIT $3 OFFSET $4) t",
    )
    .bind(status)
    .bind(pat)
    .bind(limit.unwrap_or(50).clamp(1, 200))
    .bind(offset.unwrap_or(0).max(0))
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(v)
}

/// Admin: the refund queue and history.
pub async fn list(st: &AppState, tenant: &str, h: &HeaderMap, status: Option<&str>) -> Result<Value> {
    let a = authenticate(st, tenant, h, Perm::Orders).await?;
    if let Some(s) = status {
        if !["requested", "processed", "declined", "failed"].contains(&s) {
            return Err(AppError::BadRequest("unknown status".into()));
        }
    }
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let v: Value = sqlx::query_scalar(
        "SELECT COALESCE(jsonb_agg(t), '[]'::jsonb) FROM (
           SELECT r.id, r.order_id, o.reference, o.phone, o.purpose, o.paid_with, r.amount_kobo, r.status, r.requested_by, r.reason, r.method, r.note, r.created_at, r.decided_at
             FROM refunds r JOIN orders o ON o.id = r.order_id
            WHERE $1::text IS NULL OR r.status = $1 ORDER BY r.created_at DESC LIMIT 200) t",
    )
    .bind(status)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(v)
}

#[derive(Debug, Deserialize)]
pub struct Decision {
    #[serde(default)]
    pub note: Option<String>,
}

/// Admin (operator+): turn a customer's request down.
pub async fn decline(st: &AppState, tenant: &str, h: &HeaderMap, refund_id: Uuid, d: Decision) -> Result<Value> {
    let a = authenticate(st, tenant, h, Perm::RefundDecide).await?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let n = sqlx::query("UPDATE refunds SET status = 'declined', decided_by = $2, decided_at = now(), note = $3 WHERE id = $1 AND status = 'requested'")
        .bind(refund_id)
        .bind(a.id)
        .bind(d.note.map(|n| n.chars().take(500).collect::<String>()))
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(AppError::NotFound);
    }
    audit(&mut tx, a.tenant, "admin", Some(&a.id.to_string()), "refund.declined", Some(("refund", &refund_id.to_string())), json!({})).await?;
    tx.commit().await?;
    Ok(json!({ "ok": true }))
}

/// Admin (owner): refund an order now.
pub async fn execute(st: &AppState, tenant_slug: &str, h: &HeaderMap, order_id: Uuid, d: Decision) -> Result<Value> {
    let a = authenticate(st, tenant_slug, h, Perm::RefundExecute).await?;
    let note: Option<String> = d.note.map(|n| n.trim().chars().take(500).collect());
    match run(st, &a, order_id, note.as_deref()).await {
        Ok(v) => Ok(v),
        Err(e) => {
            // Keep a trace of failed attempts (the money transaction rolled back).
            if matches!(e, AppError::Gateway(_)) {
                if let Ok(mut tx) = tenant_tx(&st.pool, a.tenant).await {
                    let _ = sqlx::query(
                        "INSERT INTO refunds (tenant_id, order_id, amount_kobo, status, requested_by, decided_by, decided_at, note)
                         SELECT $1, id, GREATEST(amount_kobo, 1), 'failed', 'admin', $3, now(), $4 FROM orders WHERE id = $2",
                    )
                    .bind(a.tenant)
                    .bind(order_id)
                    .bind(a.id)
                    .bind(e.to_string().chars().take(300).collect::<String>())
                    .execute(&mut *tx)
                    .await;
                    let _ =
                        audit(&mut tx, a.tenant, "admin", Some(&a.id.to_string()), "refund.failed", Some(("order", &order_id.to_string())), json!({})).await;
                    let _ = tx.commit().await;
                }
            }
            Err(e)
        }
    }
}

async fn run(st: &AppState, a: &crate::admin::AdminCtx, order_id: Uuid, note: Option<&str>) -> Result<Value> {
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let o = sqlx::query("SELECT reference, status::text AS status, purpose, paid_with, amount_kobo, account_id FROM orders WHERE id = $1 FOR UPDATE")
        .bind(order_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(AppError::NotFound)?;
    let status: String = o.try_get("status")?;
    if !REFUNDABLE.contains(&status.as_str()) {
        return Err(AppError::BadRequest(format!("this order is '{status}' and cannot be refunded")));
    }
    let (reference, purpose, paid_with): (String, String, String) = (o.try_get("reference")?, o.try_get("purpose")?, o.try_get("paid_with")?);
    let account: Option<Uuid> = o.try_get("account_id")?;

    // What was actually received. A `payments` row is only written after Paystack's own Verify Transaction
    // said "success"; on a wrong-amount order it is stored with verified = false (it did not match the order),
    // but the money is real, so that row is exactly what must be returned.
    if paid_with == "cash" || paid_with == "voucher" {
        return Err(AppError::BadRequest(
            "cash and voucher orders were not paid through this system, so they are refunded by hand. Cancel the voucher instead".into(),
        ));
    }
    let amount: i64 = if paid_with == "wallet" {
        o.try_get("amount_kobo")?
    } else {
        sqlx::query_scalar("SELECT amount_kobo FROM payments WHERE order_id = $1 ORDER BY received_at DESC LIMIT 1")
            .bind(order_id)
            .fetch_optional(&mut *tx)
            .await?
            .filter(|a: &i64| *a > 0)
            .ok_or_else(|| AppError::BadRequest("no successful payment is on record for this order".into()))?
    };

    // Use the customer's open request if there is one, otherwise open one now.
    let open: Option<(Uuid, String)> = sqlx::query("SELECT id, status FROM refunds WHERE order_id = $1 AND status IN ('requested', 'processed') FOR UPDATE")
        .bind(order_id)
        .fetch_optional(&mut *tx)
        .await?
        .map(|r| (r.get("id"), r.get("status")));
    let refund_id = match open {
        Some((_, s)) if s == "processed" => return Err(AppError::BadRequest("this order has already been refunded".into())),
        Some((id, _)) => id,
        None => {
            sqlx::query_scalar(
                "INSERT INTO refunds (tenant_id, order_id, amount_kobo, status, requested_by) VALUES ($1, $2, $3, 'requested', 'admin') RETURNING id",
            )
            .bind(a.tenant)
            .bind(order_id)
            .bind(amount)
            .fetch_one(&mut *tx)
            .await?
        }
    };

    let method;
    let mut provider_ref: Option<String> = None;
    if paid_with == "wallet" {
        let acct = account.ok_or_else(|| AppError::Internal("wallet order without an account".into()))?;
        sqlx::query("SELECT 1 FROM accounts WHERE id = $1 FOR UPDATE").bind(acct).fetch_one(&mut *tx).await?;
        sqlx::query("INSERT INTO wallet_ledger (tenant_id, account_id, kind, amount_kobo, order_id, note) VALUES ($1, $2, 'refund', $3, $4, 'Refund')")
            .bind(a.tenant)
            .bind(acct)
            .bind(amount)
            .bind(order_id)
            .execute(&mut *tx)
            .await?;
        method = "wallet";
    } else {
        if purpose == "topup" {
            let acct = account.ok_or_else(|| AppError::Internal("top-up without an account".into()))?;
            sqlx::query("SELECT 1 FROM accounts WHERE id = $1 FOR UPDATE").bind(acct).fetch_one(&mut *tx).await?;
            let bal: i64 = sqlx::query_scalar("SELECT COALESCE(sum(amount_kobo), 0)::bigint FROM wallet_ledger WHERE account_id = $1")
                .bind(acct)
                .fetch_one(&mut *tx)
                .await?;
            if bal < amount {
                return Err(AppError::BadRequest("the member has already spent part of this top-up, so it cannot be refunded in full".into()));
            }
            sqlx::query(
                "INSERT INTO wallet_ledger (tenant_id, account_id, kind, amount_kobo, order_id, note) VALUES ($1, $2, 'refund', $3, $4, 'Top-up refunded')",
            )
            .bind(a.tenant)
            .bind(acct)
            .bind(-amount)
            .bind(order_id)
            .execute(&mut *tx)
            .await?;
        }
        let secret = st.load_paystack_secret(&mut tx, a.tenant).await?;
        // Network call while holding the order lock: a deliberate trade, so a double click cannot double-refund.
        let r = st.gateway.refund(&secret, &reference, amount, note.unwrap_or("Refund")).await?;
        provider_ref = Some(r.provider_ref);
        method = "paystack";
    }

    // Whatever the order produced stops working now.
    let cancelled = sqlx::query("UPDATE entitlements SET status = 'cancelled' WHERE order_id = $1 AND status <> 'cancelled'")
        .bind(order_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    sqlx::query("UPDATE orders SET status = 'refunded' WHERE id = $1").bind(order_id).execute(&mut *tx).await?;
    sqlx::query("UPDATE refunds SET status = 'processed', method = $2, provider_ref = $3, decided_by = $4, decided_at = now(), note = $5, amount_kobo = $6 WHERE id = $1")
        .bind(refund_id)
        .bind(method)
        .bind(provider_ref.as_deref())
        .bind(a.id)
        .bind(note)
        .bind(amount)
        .execute(&mut *tx)
        .await?;
    audit(
        &mut tx,
        a.tenant,
        "admin",
        Some(&a.id.to_string()),
        "refund.processed",
        Some(("order", &order_id.to_string())),
        json!({ "amount_kobo": amount, "method": method, "plans_cancelled": cancelled }),
    )
    .await?;
    tx.commit().await?;
    Ok(json!({ "ok": true, "amount_kobo": amount, "method": method, "plans_cancelled": cancelled }))
}
