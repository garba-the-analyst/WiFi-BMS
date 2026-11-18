//! Vouchers issued by staff.
//!
//! * **Manual sale** (any staff member): a customer pays cash or transfer at the counter. The staff
//!   member picks a plan from the price list; the system records an order (paid_with `cash`, with the
//!   staff member's id) and either gives the customer's account the plan, or makes an access code.
//!   The price always comes from the plan, so a front-desk member cannot discount or give away access.
//! * **Special voucher** (system admin, CEO, admin; never customer reps): unlimited access with no
//!   payment, for staff, guests of the house, partners. It has no time or data budget; it is limited
//!   only by how many devices, how fast, and (optionally) when it stops. Each batch gets its own
//!   hidden plan so the terms are frozen with the voucher and never appear on the price list.
//!
//! Both paths use the same entitlement engine as a paid order, so RADIUS, accounting and the
//! customer's own page need no special cases.

use axum::http::HeaderMap;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use crate::admin::{allows, authenticate, Perm};
use crate::codes::{new_access_code, new_order_reference, pretty_code};
use crate::db::{audit, load_plan, tenant_tx};
use crate::entitlements_db::{grant, Subject};
use crate::error::{AppError, Result};
use crate::AppState;

/// "Forever" for a voucher with no end date. Ten years; the engine stores seconds in an i32.
pub const FOREVER_SECONDS: i32 = 10 * 365 * 86_400;

async fn new_code(tx: &mut PgConnection, tenant: Uuid, kind: &str, phone: Option<&str>, issued_by: Uuid, label: Option<&str>) -> Result<(Uuid, String)> {
    for _ in 0..8 {
        let code = new_access_code();
        let row = sqlx::query(
            "INSERT INTO access_codes (tenant_id, code, phone, kind, issued_by, label) VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (tenant_id, code) DO NOTHING RETURNING id",
        )
        .bind(tenant)
        .bind(&code)
        .bind(phone)
        .bind(kind)
        .bind(issued_by)
        .bind(label)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(r) = row {
            return Ok((r.try_get("id")?, code));
        }
    }
    Err(AppError::Internal("could not allocate a unique access code".into()))
}

async fn site_id(tx: &mut PgConnection, slug: &str) -> Result<Uuid> {
    sqlx::query_scalar("SELECT id FROM sites WHERE slug = $1 AND status = 'active'")
        .bind(slug)
        .fetch_optional(tx)
        .await?
        .ok_or_else(|| AppError::BadRequest("unknown site".into()))
}

#[derive(Debug, Deserialize)]
pub struct ManualSale {
    pub plan_id: Uuid,
    pub site: String,
    /// Sell to an existing member's account...
    #[serde(default)]
    pub account_id: Option<Uuid>,
    /// ...or make a voucher code for a walk-in (phone optional).
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    /// `cash` or `transfer`. Recorded in the audit log.
    #[serde(default)]
    pub method: Option<String>,
}

pub async fn manual_sale(st: &AppState, tenant: &str, h: &HeaderMap, req: ManualSale) -> Result<Value> {
    let a = authenticate(st, tenant, h, Perm::ManualSale).await?;
    let method = req.method.as_deref().unwrap_or("cash");
    if !["cash", "transfer"].contains(&method) {
        return Err(AppError::BadRequest("method must be cash or transfer".into()));
    }
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let site = site_id(&mut tx, &req.site).await?;
    let row =
        sqlx::query("SELECT is_active, is_special FROM plans WHERE id = $1").bind(req.plan_id).fetch_optional(&mut *tx).await?.ok_or(AppError::NotFound)?;
    if !row.try_get::<bool, _>("is_active")? || row.try_get::<bool, _>("is_special")? {
        return Err(AppError::BadRequest("that plan is not for sale".into()));
    }
    let plan = load_plan(&mut tx, req.plan_id).await?;
    let phone = req.phone.as_deref().map(str::trim).filter(|p| !p.is_empty()).map(|p| p.chars().take(20).collect::<String>());
    let label = req.label.as_deref().map(str::trim).filter(|p| !p.is_empty()).map(|p| p.chars().take(120).collect::<String>());
    let reference = new_order_reference();

    let (subject, code_id, code, acct_phone): (Subject, Option<Uuid>, Option<String>, Option<String>) = match req.account_id {
        Some(acct) => {
            let p: String = sqlx::query_scalar("SELECT phone FROM accounts WHERE id = $1 AND status = 'active' FOR UPDATE")
                .bind(acct)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(AppError::NotFound)?;
            (Subject::Account(acct), None, None, Some(p))
        }
        None => {
            let (id, code) = new_code(&mut tx, a.tenant, "manual", phone.as_deref(), a.id, label.as_deref()).await?;
            (Subject::Code(id), Some(id), Some(code), phone.clone())
        }
    };
    let (acct_id, order_phone) = match subject {
        Subject::Account(id) => (Some(id), acct_phone),
        Subject::Code(_) => (None, phone),
    };
    let order_id: Uuid = sqlx::query_scalar(
        "INSERT INTO orders (tenant_id, site_id, plan_id, reference, amount_kobo, account_id, access_code_id, phone, paid_with, status, paid_at, issued_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'cash', 'provisioned', now(), $9) RETURNING id",
    )
    .bind(a.tenant)
    .bind(site)
    .bind(req.plan_id)
    .bind(&reference)
    .bind(plan.price_kobo)
    .bind(acct_id)
    .bind(code_id)
    .bind(&order_phone)
    .bind(a.id)
    .fetch_one(&mut *tx)
    .await?;
    let ent = grant(&mut tx, a.tenant, order_id, req.plan_id, &plan.plan, subject).await?;
    audit(
        &mut tx,
        a.tenant,
        "admin",
        Some(&a.actor()),
        "voucher.manual_sale",
        Some(("order", &order_id.to_string())),
        json!({ "plan": plan.name, "amount_kobo": plan.price_kobo, "method": method, "to": if acct_id.is_some() { "account" } else { "code" }, "label": label }),
    )
    .await?;
    tx.commit().await?;
    Ok(json!({
        "reference": reference, "entitlement_id": ent, "plan": plan.name, "amount_kobo": plan.price_kobo,
        "access_code": code.as_deref().map(pretty_code),
    }))
}

#[derive(Debug, Deserialize)]
pub struct Duration {
    pub unit: String,
    pub count: u32,
}

impl Duration {
    pub fn seconds(&self) -> Result<i64> {
        let unit = match self.unit.as_str() {
            "hour" => 3600,
            "day" => 86_400,
            "week" => 7 * 86_400,
            "month" => 30 * 86_400,
            _ => return Err(AppError::BadRequest("unit must be hour, day, week or month".into())),
        };
        if self.count == 0 || self.count > 1200 {
            return Err(AppError::BadRequest("the number must be between 1 and 1200".into()));
        }
        Ok(unit * self.count as i64)
    }
}

#[derive(Debug, Deserialize)]
pub struct SpecialVoucher {
    /// Who or what it is for. Required, so every unlimited voucher can be explained later.
    pub label: String,
    pub site: String,
    #[serde(default = "one")]
    pub devices: i16,
    /// Stop working after this long (counted from first use). Leave out for no end date.
    #[serde(default)]
    pub valid_for: Option<Duration>,
    #[serde(default)]
    pub speed_profile_id: Option<Uuid>,
    #[serde(default)]
    pub down_kbps: Option<i32>,
    #[serde(default)]
    pub up_kbps: Option<i32>,
    /// How many codes to make (1 to 50).
    #[serde(default = "one_usize")]
    pub count: usize,
}
fn one() -> i16 {
    1
}
fn one_usize() -> usize {
    1
}

pub async fn special_voucher(st: &AppState, tenant: &str, h: &HeaderMap, req: SpecialVoucher) -> Result<Value> {
    let a = authenticate(st, tenant, h, Perm::SpecialVoucher).await?;
    let label = req.label.trim();
    if label.is_empty() || label.len() > 120 {
        return Err(AppError::BadRequest("say who the voucher is for (up to 120 characters)".into()));
    }
    if !(1..=50).contains(&req.count) || !(1..=10).contains(&req.devices) {
        return Err(AppError::BadRequest("make 1 to 50 codes, for 1 to 10 devices".into()));
    }
    let validity: i32 = match &req.valid_for {
        Some(d) => i32::try_from(d.seconds()?.min(FOREVER_SECONDS as i64)).unwrap_or(FOREVER_SECONDS),
        None => FOREVER_SECONDS,
    };
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let site = site_id(&mut tx, &req.site).await?;
    let profile = crate::admin::resolve_profile(&mut tx, a.tenant, req.speed_profile_id, req.down_kbps, req.up_kbps).await?;
    let plan_name = format!("Special: {} ({})", label.chars().take(60).collect::<String>(), &Uuid::new_v4().simple().to_string()[..6]);
    // No time or data budget worth the name: the active-time allowance equals the whole validity window.
    let plan_id: Uuid = sqlx::query_scalar(
        "INSERT INTO plans (tenant_id, name, kind, price_kobo, active_seconds, valid_seconds, activate_on_first_use, unused_expiry_seconds,
                            device_limit, speed_profile_id, is_active, is_special, description)
         VALUES ($1, $2, 'time', 0, $3, $3, true, $3, $4, $5, false, true, 'Unlimited voucher issued by staff') RETURNING id",
    )
    .bind(a.tenant)
    .bind(&plan_name)
    .bind(validity)
    .bind(req.devices)
    .bind(profile)
    .fetch_one(&mut *tx)
    .await?;
    let plan = load_plan(&mut tx, plan_id).await.map_err(|e| match e {
        AppError::Internal(m) => AppError::BadRequest(format!("this voucher is not valid: {m}")),
        o => o,
    })?;
    let mut out = Vec::new();
    for _ in 0..req.count {
        let (code_id, code) = new_code(&mut tx, a.tenant, "special", None, a.id, Some(label)).await?;
        let reference = new_order_reference();
        let order_id: Uuid = sqlx::query_scalar(
            "INSERT INTO orders (tenant_id, site_id, plan_id, reference, amount_kobo, access_code_id, paid_with, status, paid_at, issued_by)
             VALUES ($1, $2, $3, $4, 0, $5, 'voucher', 'provisioned', now(), $6) RETURNING id",
        )
        .bind(a.tenant)
        .bind(site)
        .bind(plan_id)
        .bind(&reference)
        .bind(code_id)
        .bind(a.id)
        .fetch_one(&mut *tx)
        .await?;
        let ent = grant(&mut tx, a.tenant, order_id, plan_id, &plan.plan, Subject::Code(code_id)).await?;
        out.push(json!({ "access_code": pretty_code(&code), "entitlement_id": ent }));
    }
    audit(
        &mut tx,
        a.tenant,
        "admin",
        Some(&a.actor()),
        "voucher.special_created",
        Some(("plan", &plan_id.to_string())),
        json!({ "label": label, "count": req.count, "devices": req.devices, "valid_seconds": validity, "forever": req.valid_for.is_none() }),
    )
    .await?;
    tx.commit().await?;
    Ok(json!({ "label": label, "vouchers": out, "ends": if req.valid_for.is_some() { "after the chosen period from first use" } else { "never" } }))
}

/// Staff list: who made which code, and whether it has been used. Special vouchers are shown only to
/// those allowed to make them.
pub async fn list(st: &AppState, tenant: &str, h: &HeaderMap, kind: Option<&str>, q: Option<&str>, limit: Option<i64>) -> Result<Value> {
    let a = authenticate(st, tenant, h, Perm::ManualSale).await?;
    if let Some(k) = kind {
        if !["manual", "special", "paid"].contains(&k) {
            return Err(AppError::BadRequest("kind must be manual, special or paid".into()));
        }
        if k == "special" && !allows(a.role, Perm::SpecialVoucher) {
            return Err(AppError::Forbidden);
        }
    }
    let see_special = allows(a.role, Perm::SpecialVoucher);
    let pat = format!("%{}%", q.unwrap_or("").trim().replace(['%', '_'], "").to_ascii_uppercase());
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let v: Value = sqlx::query_scalar(
        "SELECT COALESCE(jsonb_agg(t), '[]'::jsonb) FROM (
           SELECT c.id, c.code, c.kind, c.label, c.phone, c.created_at, c.revoked_at, u.email AS issued_by,
                  e.status::text AS status, e.expires_at, p.name AS plan, p.device_limit
             FROM access_codes c
             LEFT JOIN admin_users u ON u.id = c.issued_by
             LEFT JOIN LATERAL (SELECT * FROM entitlements x WHERE x.access_code_id = c.id ORDER BY x.granted_at DESC LIMIT 1) e ON true
             LEFT JOIN plans p ON p.id = e.plan_id
            WHERE c.kind <> 'paid' AND ($1::text IS NULL OR c.kind = $1) AND ($2 OR c.kind <> 'special')
              AND (c.code LIKE $3 OR upper(COALESCE(c.label, '')) LIKE $3 OR COALESCE(c.phone, '') LIKE $3)
            ORDER BY c.created_at DESC LIMIT $4) t",
    )
    .bind(kind)
    .bind(see_special)
    .bind(pat)
    .bind(limit.unwrap_or(100).clamp(1, 500))
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(v)
}

/// Switch a staff-issued voucher off now. Anyone connected with it is dropped at the next accounting update.
pub async fn revoke(st: &AppState, tenant: &str, h: &HeaderMap, id: Uuid) -> Result<Value> {
    let a = authenticate(st, tenant, h, Perm::SpecialVoucher).await?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let kind: String = sqlx::query_scalar("SELECT kind FROM access_codes WHERE id = $1 AND kind <> 'paid' FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(AppError::NotFound)?;
    sqlx::query("UPDATE access_codes SET revoked_at = COALESCE(revoked_at, now()) WHERE id = $1").bind(id).execute(&mut *tx).await?;
    let n = sqlx::query("UPDATE entitlements SET status = 'cancelled' WHERE access_code_id = $1 AND status <> 'cancelled'")
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    audit(
        &mut tx,
        a.tenant,
        "admin",
        Some(&a.actor()),
        "voucher.revoked",
        Some(("access_code", &id.to_string())),
        json!({ "kind": kind, "plans_cancelled": n }),
    )
    .await?;
    tx.commit().await?;
    Ok(json!({ "ok": true, "plans_cancelled": n }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_convert_and_are_bounded() {
        assert_eq!(Duration { unit: "hour".into(), count: 2 }.seconds().unwrap(), 7200);
        assert_eq!(Duration { unit: "month".into(), count: 1 }.seconds().unwrap(), 30 * 86_400);
        assert!(Duration { unit: "year".into(), count: 1 }.seconds().is_err());
        assert!(Duration { unit: "day".into(), count: 0 }.seconds().is_err());
        assert!(Duration { unit: "day".into(), count: 5000 }.seconds().is_err());
    }

    #[test]
    fn forever_fits_the_engine() {
        assert!(FOREVER_SECONDS > 0 && (FOREVER_SECONDS as i64) < i32::MAX as i64);
    }
}
