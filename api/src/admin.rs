//! Operator back office: sign-in with 2-step verification, roles, analytics,
//! customers, plans, network, security, audit export and secrets.
//!
//! Roles: `viewer` reads, `operator` also edits plans and speed profiles,
//! `owner` also manages node tokens, the Paystack key and other admins.
//!
//! Every change is written to the tamper-evident audit log with the admin's id.
//! Read endpoints build their JSON inside PostgreSQL (`jsonb_agg`), so the
//! shape of each report is visible in its SQL.

use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use crate::auth::{bearer, check_password_policy, dummy_verify, hash_password, new_token, sha256_hex, verify_password};
use crate::db::{audit, load_plan, tenant_id_by_slug, tenant_tx};
use crate::error::{AppError, Result};
use crate::totp;
use crate::AppState;

pub const SESSION_HOURS: i32 = 12;
const MAX_FAILED: i32 = 5;
const LOCK_MINUTES: i32 = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Viewer,
    Operator,
    Owner,
}

fn role_from(s: &str) -> Role {
    match s {
        "owner" => Role::Owner,
        "operator" => Role::Operator,
        _ => Role::Viewer,
    }
}

#[derive(Debug, Clone)]
pub struct AdminCtx {
    pub tenant: Uuid,
    pub id: Uuid,
    pub email: String,
    pub role: Role,
}

impl AdminCtx {
    fn need(&self, min: Role) -> Result<()> {
        if self.role >= min {
            Ok(())
        } else {
            Err(AppError::Forbidden)
        }
    }
    fn actor(&self) -> String {
        self.id.to_string()
    }
}

/// Resolve the bearer token to an admin, requiring at least `min`.
pub async fn authenticate(st: &AppState, tenant_slug: &str, headers: &HeaderMap, min: Role) -> Result<AdminCtx> {
    let token = bearer(headers).ok_or(AppError::Unauthorised)?;
    if !token.starts_with("at_") {
        return Err(AppError::Unauthorised);
    }
    let tenant = tenant_id_by_slug(&st.pool, tenant_slug).await?;
    let mut tx = tenant_tx(&st.pool, tenant).await?;
    let row = sqlx::query(
        "UPDATE admin_sessions s SET last_used_at = now()
           FROM admin_users u
          WHERE s.token_hash = $1 AND s.revoked_at IS NULL AND s.expires_at > now()
            AND u.id = s.admin_user_id AND u.status = 'active'
        RETURNING u.id, u.email, u.role::text AS role",
    )
    .bind(sha256_hex(token))
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::Unauthorised)?;
    tx.commit().await?;
    let ctx = AdminCtx { tenant, id: row.try_get("id")?, email: row.try_get("email")?, role: role_from(&row.try_get::<String, _>("role")?) };
    ctx.need(min)?;
    Ok(ctx)
}

fn ip_of(st: &AppState, h: &HeaderMap) -> Option<String> {
    crate::auth::client_ip(h, st.config.trust_proxy)
}

// ---------------------------------------------------------------------------
// Sign-in, 2-step verification
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct Login {
    pub email: String,
    pub password: String,
    #[serde(default)]
    pub totp: Option<String>,
}

enum Outcome {
    Ok(Uuid, bool, String),
    Bad,
    Locked,
}

pub async fn login(st: &AppState, tenant_slug: &str, headers: &HeaderMap, req: Login) -> Result<Value> {
    let ip = ip_of(st, headers);
    st.limiter.hit(&format!("admin-ip:{}", ip.as_deref().unwrap_or("?")), 20, std::time::Duration::from_secs(600))?;
    let email = req.email.trim().to_ascii_lowercase();
    st.limiter.hit(&format!("admin:{tenant_slug}:{email}"), 10, std::time::Duration::from_secs(600))?;
    if req.password.len() > 256 || email.len() > 200 {
        return Err(AppError::Unauthorised);
    }
    let tenant = tenant_id_by_slug(&st.pool, tenant_slug).await?;
    let mut tx = tenant_tx(&st.pool, tenant).await?;
    let row = sqlx::query(
        "SELECT id, password_hash, totp_secret, totp_enabled, status::text AS status, role::text AS role,
                (locked_until IS NOT NULL AND locked_until > now()) AS locked
           FROM admin_users WHERE lower(email) = $1 FOR UPDATE",
    )
    .bind(&email)
    .fetch_optional(&mut *tx)
    .await?;

    let outcome = match row {
        None => {
            dummy_verify(&req.password);
            Outcome::Bad
        }
        Some(r) => {
            let id: Uuid = r.try_get("id")?;
            let hash: String = r.try_get("password_hash")?;
            let enabled: bool = r.try_get("totp_enabled")?;
            let secret: Option<Vec<u8>> = r.try_get("totp_secret")?;
            let active = r.try_get::<String, _>("status")? == "active";
            if r.try_get::<bool, _>("locked")? {
                Outcome::Locked
            } else if verify_password(&hash, &req.password) && active && second_step_ok(st, enabled, secret.as_deref(), req.totp.as_deref())? {
                sqlx::query("UPDATE admin_users SET failed_logins = 0, locked_until = NULL, last_login_at = now() WHERE id = $1")
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                Outcome::Ok(id, enabled, r.try_get("role")?)
            } else {
                sqlx::query(
                    "UPDATE admin_users SET failed_logins = CASE WHEN failed_logins + 1 >= $2 THEN 0 ELSE failed_logins + 1 END,
                            locked_until = CASE WHEN failed_logins + 1 >= $2 THEN now() + make_interval(mins => $3) ELSE locked_until END
                      WHERE id = $1",
                )
                .bind(id)
                .bind(MAX_FAILED)
                .bind(LOCK_MINUTES)
                .execute(&mut *tx)
                .await?;
                audit(&mut tx, tenant, "admin", Some(&id.to_string()), "admin.login_failed", Some(("admin", &id.to_string())), json!({})).await?;
                Outcome::Bad
            }
        }
    };
    match outcome {
        Outcome::Ok(id, enrolled, role) => {
            let token = new_token("at");
            sqlx::query(
                "INSERT INTO admin_sessions (tenant_id, admin_user_id, token_hash, expires_at, ip, user_agent)
                 VALUES ($1, $2, $3, now() + make_interval(hours => $4), $5::inet, $6)",
            )
            .bind(tenant)
            .bind(id)
            .bind(sha256_hex(&token))
            .bind(SESSION_HOURS)
            .bind(ip.as_deref())
            .bind(headers.get("user-agent").and_then(|v| v.to_str().ok()).map(|s| s.chars().take(300).collect::<String>()))
            .execute(&mut *tx)
            .await?;
            audit(&mut tx, tenant, "admin", Some(&id.to_string()), "admin.login", Some(("admin", &id.to_string())), json!({ "totp": enrolled })).await?;
            tx.commit().await?;
            Ok(json!({ "token": token, "role": role, "totp_enrolled": enrolled, "expires_in_hours": SESSION_HOURS }))
        }
        Outcome::Bad => {
            tx.commit().await?;
            Err(AppError::Unauthorised)
        }
        Outcome::Locked => {
            tx.commit().await?;
            Err(AppError::RateLimited)
        }
    }
}

/// Not enrolled: no second step required (the admin is told to enrol).
/// Enrolled: a correct current code is required.
fn second_step_ok(st: &AppState, enabled: bool, secret_enc: Option<&[u8]>, code: Option<&str>) -> Result<bool> {
    if !enabled {
        return Ok(true);
    }
    let (Some(enc), Some(code)) = (secret_enc, code) else { return Ok(false) };
    let secret = st.master_key.decrypt(std::str::from_utf8(enc).map_err(|_| AppError::Internal("totp secret unreadable".into()))?)?;
    let bytes = totp::base32_decode(&secret).ok_or_else(|| AppError::Internal("totp secret unreadable".into()))?;
    Ok(totp::verify(&bytes, crate::entitlements_db::now_secs() as u64, code).is_some())
}

pub async fn logout(st: &AppState, tenant_slug: &str, headers: &HeaderMap) -> Result<Value> {
    let a = authenticate(st, tenant_slug, headers, Role::Viewer).await?;
    let token = bearer(headers).ok_or(AppError::Unauthorised)?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    sqlx::query("UPDATE admin_sessions SET revoked_at = now() WHERE token_hash = $1").bind(sha256_hex(token)).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(json!({ "ok": true }))
}

/// Step 1 of enrolment: make a secret and return the QR/URI. Not active until confirmed.
pub async fn totp_setup(st: &AppState, tenant_slug: &str, headers: &HeaderMap) -> Result<Value> {
    let a = authenticate(st, tenant_slug, headers, Role::Viewer).await?;
    let raw = totp::new_secret();
    let enc = st.master_key.encrypt(&totp::base32_encode(&raw))?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    sqlx::query("UPDATE admin_users SET totp_secret = $2, totp_enabled = false WHERE id = $1").bind(a.id).bind(enc.as_bytes()).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(json!({ "otpauth_uri": totp::provisioning_uri("WiFi-BMS", &a.email, &raw), "secret": totp::base32_encode(&raw) }))
}

#[derive(Debug, Deserialize)]
pub struct Confirm {
    pub code: String,
}

/// Step 2: prove the authenticator app works, then turn it on.
pub async fn totp_confirm(st: &AppState, tenant_slug: &str, headers: &HeaderMap, req: Confirm) -> Result<Value> {
    let a = authenticate(st, tenant_slug, headers, Role::Viewer).await?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let enc: Option<Vec<u8>> = sqlx::query_scalar("SELECT totp_secret FROM admin_users WHERE id = $1").bind(a.id).fetch_one(&mut *tx).await?;
    if !second_step_ok(st, true, enc.as_deref(), Some(&req.code))? {
        return Err(AppError::BadRequest("that code is not right. Check your phone's clock and try again".into()));
    }
    sqlx::query("UPDATE admin_users SET totp_enabled = true WHERE id = $1").bind(a.id).execute(&mut *tx).await?;
    audit(&mut tx, a.tenant, "admin", Some(&a.actor()), "admin.totp_enabled", Some(("admin", &a.actor())), json!({})).await?;
    tx.commit().await?;
    Ok(json!({ "ok": true }))
}

/// Used by the `create-admin` command (see main.rs) and tests.
pub async fn create_admin(st: &AppState, tenant_slug: &str, email: &str, password: &str, role: &str) -> Result<Uuid> {
    if !["owner", "operator", "viewer"].contains(&role) {
        return Err(AppError::BadRequest("role must be owner, operator or viewer".into()));
    }
    check_password_policy(password, "")?;
    let tenant = tenant_id_by_slug(&st.pool, tenant_slug).await?;
    let mut tx = tenant_tx(&st.pool, tenant).await?;
    let id: Uuid =
        sqlx::query_scalar("INSERT INTO admin_users (tenant_id, email, password_hash, role) VALUES ($1, lower($2), $3, $4::admin_role) RETURNING id")
            .bind(tenant)
            .bind(email.trim())
            .bind(hash_password(password)?)
            .bind(role)
            .fetch_one(&mut *tx)
            .await?;
    audit(&mut tx, tenant, "system", None, "admin.created", Some(("admin", &id.to_string())), json!({ "role": role })).await?;
    tx.commit().await?;
    Ok(id)
}

// ---------------------------------------------------------------------------
// Reports
// ---------------------------------------------------------------------------

/// Fixed set, so the values can safely be placed in SQL text.
fn range_of(q: Option<&str>) -> Result<(&'static str, &'static str)> {
    Ok(match q.unwrap_or("7d") {
        "24h" => ("24 hours", "hour"),
        "7d" => ("7 days", "day"),
        "30d" => ("30 days", "day"),
        "90d" => ("90 days", "day"),
        _ => return Err(AppError::BadRequest("range must be 24h, 7d, 30d or 90d".into())),
    })
}

async fn json_of(conn: &mut PgConnection, sql: &str) -> Result<Value> {
    Ok(sqlx::query_scalar::<_, Value>(&format!("SELECT COALESCE(jsonb_agg(t), '[]'::jsonb) FROM ({sql}) t")).fetch_one(conn).await?)
}

pub async fn overview(st: &AppState, tenant: &str, h: &HeaderMap, range: Option<&str>) -> Result<Value> {
    let a = authenticate(st, tenant, h, Role::Viewer).await?;
    let (interval, _) = range_of(range)?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let v: Value = sqlx::query_scalar(&format!(
        "SELECT jsonb_build_object(
           'plan_sales_kobo', COALESCE((SELECT sum(amount_kobo) FROM orders WHERE purpose = 'plan' AND status IN ('paid','provisioned') AND paid_at > now() - interval '{interval}'), 0),
           'cash_in_kobo',    COALESCE((SELECT sum(amount_kobo) FROM orders WHERE paid_with = 'paystack' AND status IN ('paid','provisioned') AND paid_at > now() - interval '{interval}'), 0),
           'orders_paid',     (SELECT count(*) FROM orders WHERE status IN ('paid','provisioned') AND paid_at > now() - interval '{interval}'),
           'orders_refund_flagged', (SELECT count(*) FROM orders WHERE status = 'refund_flagged'),
           'online_now',      (SELECT count(*) FROM sessions WHERE ended_at IS NULL AND last_update_at > now() - interval '5 minutes'),
           'new_members',     (SELECT count(*) FROM accounts WHERE created_at > now() - interval '{interval}'),
           'bytes_in',        COALESCE((SELECT sum(bytes_in) FROM sessions WHERE started_at > now() - interval '{interval}'), 0),
           'bytes_out',       COALESCE((SELECT sum(bytes_out) FROM sessions WHERE started_at > now() - interval '{interval}'), 0),
           'tether_flags',    (SELECT count(*) FROM tethering_flags WHERE ts > now() - interval '{interval}'),
           'wallet_float_kobo', COALESCE((SELECT sum(amount_kobo) FROM wallet_ledger), 0)
         )"
    ))
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(v)
}

pub async fn analytics(st: &AppState, tenant: &str, h: &HeaderMap, what: &str, range: Option<&str>) -> Result<Value> {
    let a = authenticate(st, tenant, h, Role::Viewer).await?;
    let (interval, bucket) = range_of(range)?;
    let tz = "Africa/Lagos";
    let sql = match what {
        "revenue" => format!(
            "SELECT date_trunc('{bucket}', paid_at AT TIME ZONE '{tz}') AS bucket,
                    sum(amount_kobo) FILTER (WHERE purpose = 'plan') AS plan_sales_kobo,
                    sum(amount_kobo) FILTER (WHERE purpose = 'topup') AS topups_kobo,
                    count(*) FILTER (WHERE purpose = 'plan') AS plans_sold
               FROM orders WHERE status IN ('paid','provisioned') AND paid_at > now() - interval '{interval}'
              GROUP BY 1 ORDER BY 1"
        ),
        "traffic" => format!(
            "SELECT date_trunc('{bucket}', started_at AT TIME ZONE '{tz}') AS bucket,
                    sum(bytes_in)::bigint AS bytes_in, sum(bytes_out)::bigint AS bytes_out,
                    sum(active_seconds)::bigint AS active_seconds, count(*) AS sessions
               FROM sessions WHERE started_at > now() - interval '{interval}'
              GROUP BY 1 ORDER BY 1"
        ),
        "devices" => format!(
            "SELECT COALESCE(d.os_family, 'Unknown') AS os, COALESCE(d.device_class, 'unknown') AS class, count(DISTINCT d.id) AS devices
               FROM sessions s JOIN devices d ON d.id = s.device_id
              WHERE s.started_at > now() - interval '{interval}'
              GROUP BY 1, 2 ORDER BY 3 DESC"
        ),
        "plans" => format!(
            "SELECT p.name, count(*) AS sold, sum(o.amount_kobo) AS revenue_kobo
               FROM orders o JOIN plans p ON p.id = o.plan_id
              WHERE o.purpose = 'plan' AND o.status IN ('paid','provisioned') AND o.paid_at > now() - interval '{interval}'
              GROUP BY 1 ORDER BY 3 DESC"
        ),
        _ => return Err(AppError::NotFound),
    };
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let v = json_of(&mut tx, &sql).await?;
    tx.commit().await?;
    Ok(v)
}

pub async fn customers(st: &AppState, tenant: &str, h: &HeaderMap, q: Option<&str>, limit: Option<i64>, offset: Option<i64>) -> Result<Value> {
    let a = authenticate(st, tenant, h, Role::Viewer).await?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let pat = format!("%{}%", q.unwrap_or("").trim().replace(['%', '_'], ""));
    let v: Value = sqlx::query_scalar(
        "SELECT COALESCE(jsonb_agg(t), '[]'::jsonb) FROM (
           SELECT a.id, a.phone, a.display_name, a.status::text AS status, a.created_at, a.last_login_at,
                  COALESCE((SELECT sum(amount_kobo) FROM wallet_ledger w WHERE w.account_id = a.id), 0) AS balance_kobo,
                  (SELECT count(*) FROM entitlements e WHERE e.account_id = a.id AND e.status IN ('pending','active')) AS live_plans
             FROM accounts a WHERE a.phone ILIKE $1 OR COALESCE(a.display_name, '') ILIKE $1
            ORDER BY a.created_at DESC LIMIT $2 OFFSET $3) t",
    )
    .bind(pat)
    .bind(limit.unwrap_or(50).clamp(1, 200))
    .bind(offset.unwrap_or(0).max(0))
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(v)
}

pub async fn customer_detail(st: &AppState, tenant: &str, h: &HeaderMap, id: Uuid) -> Result<Value> {
    let a = authenticate(st, tenant, h, Role::Viewer).await?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let acct = json_of(
        &mut tx,
        &format!("SELECT id, phone, display_name, email, status::text AS status, created_at, last_login_at, failed_logins FROM accounts WHERE id = '{id}'"),
    )
    .await?;
    if acct.as_array().is_none_or(|x| x.is_empty()) {
        return Err(AppError::NotFound);
    }
    let ents = json_of(
        &mut tx,
        &format!(
            "SELECT e.id, p.name AS plan, e.status::text AS status, e.granted_at, e.activated_at, e.expires_at, e.remaining_active_seconds, e.remaining_bytes
           FROM entitlements e JOIN plans p ON p.id = e.plan_id WHERE e.account_id = '{id}' ORDER BY e.granted_at DESC LIMIT 50"
        ),
    )
    .await?;
    let sessions = json_of(
        &mut tx,
        &format!(
            "SELECT s.id, s.started_at, s.ended_at, s.end_reason::text AS end_reason, s.bytes_in, s.bytes_out, s.active_seconds, s.ip::text AS ip, s.ssid,
                d.mac::text AS mac, d.os_family, d.device_class, d.hostname
           FROM sessions s JOIN entitlements e ON e.id = s.entitlement_id JOIN devices d ON d.id = s.device_id
          WHERE e.account_id = '{id}' ORDER BY s.started_at DESC LIMIT 100"
        ),
    )
    .await?;
    let devices = json_of(&mut tx, &format!(
        "SELECT DISTINCT d.id, d.mac::text AS mac, d.mac_randomised, d.os_family, d.os_version, d.device_class, d.hostname, d.confidence, d.first_seen_at, d.last_seen_at
           FROM device_bindings b JOIN entitlements e ON e.id = b.entitlement_id JOIN devices d ON d.id = b.device_id WHERE e.account_id = '{id}'")).await?;
    let flags = json_of(
        &mut tx,
        &format!(
            "SELECT f.ts, f.signal, f.score, f.action, f.detail FROM tethering_flags f
           WHERE f.device_id IN (SELECT b.device_id FROM device_bindings b JOIN entitlements e ON e.id = b.entitlement_id WHERE e.account_id = '{id}')
           ORDER BY f.ts DESC LIMIT 50"
        ),
    )
    .await?;
    let history = json_of(&mut tx, &format!("SELECT seq, ts, actor_kind, action, detail FROM audit_log WHERE actor_id = '{id}' OR (entity = 'account' AND entity_id = '{id}') ORDER BY seq DESC LIMIT 100")).await?;
    tx.commit().await?;
    Ok(json!({ "account": acct[0], "entitlements": ents, "sessions": sessions, "devices": devices, "tether_flags": flags, "audit": history }))
}

// ---------------------------------------------------------------------------
// Plans and speed profiles
// ---------------------------------------------------------------------------

pub async fn list_plans(st: &AppState, tenant: &str, h: &HeaderMap) -> Result<Value> {
    let a = authenticate(st, tenant, h, Role::Viewer).await?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let v = json_of(
        &mut tx,
        "SELECT p.id, p.name, p.kind::text AS kind, p.price_kobo, p.active_seconds, p.data_bytes, p.valid_seconds, p.activate_on_first_use,
            p.device_limit, p.swap_cooldown_s, p.fup_daily_bytes, p.is_active, s.name AS speed_profile, s.down_kbps, s.up_kbps
        FROM plans p JOIN speed_profiles s ON s.id = p.speed_profile_id ORDER BY p.price_kobo, p.name",
    )
    .await?;
    tx.commit().await?;
    Ok(v)
}

#[derive(Debug, Deserialize)]
pub struct PlanIn {
    pub name: String,
    pub kind: String,
    pub price_kobo: i64,
    #[serde(default)]
    pub active_seconds: Option<i32>,
    #[serde(default)]
    pub data_bytes: Option<i64>,
    pub valid_seconds: i32,
    #[serde(default = "yes")]
    pub activate_on_first_use: bool,
    #[serde(default = "one")]
    pub device_limit: i16,
    #[serde(default = "day")]
    pub swap_cooldown_s: i32,
    pub speed_profile_id: Uuid,
    #[serde(default)]
    pub fup_daily_bytes: Option<i64>,
    #[serde(default)]
    pub fup_speed_profile_id: Option<Uuid>,
}
fn yes() -> bool {
    true
}
fn one() -> i16 {
    1
}
fn day() -> i32 {
    86_400
}

/// Create (id = None) or replace (id = Some) a plan. The stored row is
/// loaded back through the entitlement engine, so a plan that the engine
/// would reject is never saved. Existing purchases keep their own snapshot.
pub async fn save_plan(st: &AppState, tenant: &str, h: &HeaderMap, id: Option<Uuid>, p: PlanIn) -> Result<Value> {
    let a = authenticate(st, tenant, h, Role::Operator).await?;
    if !["time", "data", "hybrid"].contains(&p.kind.as_str()) {
        return Err(AppError::BadRequest("kind must be time, data or hybrid".into()));
    }
    if p.name.trim().is_empty() || p.name.len() > 80 || p.price_kobo < 0 {
        return Err(AppError::BadRequest("give the plan a name and a price of zero or more".into()));
    }
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let sql_vals = "name=$2, kind=$3::plan_kind, price_kobo=$4, active_seconds=$5, data_bytes=$6, valid_seconds=$7, activate_on_first_use=$8,
                    device_limit=$9, swap_cooldown_s=$10, speed_profile_id=$11, fup_daily_bytes=$12, fup_speed_profile_id=$13";
    let plan_id = match id {
        Some(id) => {
            let n = sqlx::query(&format!("UPDATE plans SET {sql_vals} WHERE id = $1")).bind(id).bind(p.name.trim()).bind(&p.kind).bind(p.price_kobo).bind(p.active_seconds).bind(p.data_bytes)
                .bind(p.valid_seconds).bind(p.activate_on_first_use).bind(p.device_limit).bind(p.swap_cooldown_s).bind(p.speed_profile_id).bind(p.fup_daily_bytes).bind(p.fup_speed_profile_id)
                .execute(&mut *tx).await.map_err(plan_db_err)?;
            if n.rows_affected() == 0 {
                return Err(AppError::NotFound);
            }
            id
        }
        None => sqlx::query_scalar(
            "INSERT INTO plans (tenant_id, name, kind, price_kobo, active_seconds, data_bytes, valid_seconds, activate_on_first_use, device_limit, swap_cooldown_s, speed_profile_id, fup_daily_bytes, fup_speed_profile_id)
             VALUES ($1, $2, $3::plan_kind, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) RETURNING id",
        )
        .bind(a.tenant).bind(p.name.trim()).bind(&p.kind).bind(p.price_kobo).bind(p.active_seconds).bind(p.data_bytes).bind(p.valid_seconds).bind(p.activate_on_first_use)
        .bind(p.device_limit).bind(p.swap_cooldown_s).bind(p.speed_profile_id).bind(p.fup_daily_bytes).bind(p.fup_speed_profile_id)
        .fetch_one(&mut *tx).await.map_err(plan_db_err)?,
    };
    // Engine validation: Internal here means the stored plan failed `Plan::validate`.
    load_plan(&mut tx, plan_id).await.map_err(|e| match e {
        AppError::Internal(m) => AppError::BadRequest(format!("this plan is not valid: {m}")),
        // The joined speed profile is invisible (not this tenant's): treat as unknown.
        AppError::NotFound => AppError::BadRequest("that speed profile does not exist".into()),
        other => other,
    })?;
    audit(
        &mut tx,
        a.tenant,
        "admin",
        Some(&a.actor()),
        if id.is_some() { "plan.updated" } else { "plan.created" },
        Some(("plan", &plan_id.to_string())),
        json!({ "name": p.name, "price_kobo": p.price_kobo }),
    )
    .await?;
    tx.commit().await?;
    Ok(json!({ "id": plan_id }))
}

fn plan_db_err(e: sqlx::Error) -> AppError {
    match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => AppError::BadRequest("a plan with that name already exists".into()),
        sqlx::Error::Database(d) if d.is_check_violation() || d.is_foreign_key_violation() => {
            AppError::BadRequest("the plan's numbers or speed profile are not valid".into())
        }
        _ => e.into(),
    }
}

pub async fn set_plan_active(st: &AppState, tenant: &str, h: &HeaderMap, id: Uuid, active: bool) -> Result<Value> {
    let a = authenticate(st, tenant, h, Role::Operator).await?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let n = sqlx::query("UPDATE plans SET is_active = $2 WHERE id = $1").bind(id).bind(active).execute(&mut *tx).await?.rows_affected();
    if n == 0 {
        return Err(AppError::NotFound);
    }
    audit(&mut tx, a.tenant, "admin", Some(&a.actor()), "plan.toggled", Some(("plan", &id.to_string())), json!({ "active": active })).await?;
    tx.commit().await?;
    Ok(json!({ "ok": true }))
}

pub async fn list_profiles(st: &AppState, tenant: &str, h: &HeaderMap) -> Result<Value> {
    let a = authenticate(st, tenant, h, Role::Viewer).await?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let v = json_of(&mut tx, "SELECT id, name, down_kbps, up_kbps, burst_down_kbps, burst_up_kbps, burst_threshold_kbps, burst_time_s, priority FROM speed_profiles ORDER BY down_kbps").await?;
    tx.commit().await?;
    Ok(v)
}

#[derive(Debug, Deserialize)]
pub struct ProfileIn {
    pub name: String,
    pub down_kbps: i32,
    pub up_kbps: i32,
    #[serde(default)]
    pub burst_down_kbps: Option<i32>,
    #[serde(default)]
    pub burst_up_kbps: Option<i32>,
    #[serde(default)]
    pub burst_threshold_kbps: Option<i32>,
    #[serde(default)]
    pub burst_time_s: Option<i32>,
    #[serde(default = "eight")]
    pub priority: i16,
}
fn eight() -> i16 {
    8
}

pub async fn create_profile(st: &AppState, tenant: &str, h: &HeaderMap, p: ProfileIn) -> Result<Value> {
    let a = authenticate(st, tenant, h, Role::Operator).await?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO speed_profiles (tenant_id, name, down_kbps, up_kbps, burst_down_kbps, burst_up_kbps, burst_threshold_kbps, burst_time_s, priority)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING id",
    )
    .bind(a.tenant)
    .bind(p.name.trim())
    .bind(p.down_kbps)
    .bind(p.up_kbps)
    .bind(p.burst_down_kbps)
    .bind(p.burst_up_kbps)
    .bind(p.burst_threshold_kbps)
    .bind(p.burst_time_s)
    .bind(p.priority)
    .fetch_one(&mut *tx)
    .await
    .map_err(plan_db_err)?;
    audit(&mut tx, a.tenant, "admin", Some(&a.actor()), "speed_profile.created", Some(("speed_profile", &id.to_string())), json!({ "name": p.name })).await?;
    tx.commit().await?;
    Ok(json!({ "id": id }))
}

// ---------------------------------------------------------------------------
// Network and node tokens
// ---------------------------------------------------------------------------

pub async fn network(st: &AppState, tenant: &str, h: &HeaderMap) -> Result<Value> {
    let a = authenticate(st, tenant, h, Role::Viewer).await?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let sites = json_of(
        &mut tx,
        "SELECT s.id, s.name, s.slug, s.status,
            (SELECT count(*) FROM sessions x WHERE x.site_id = s.id AND x.ended_at IS NULL AND x.last_update_at > now() - interval '5 minutes') AS online_now,
            (SELECT max(m.ts) FROM metrics m WHERE m.site_id = s.id AND m.metric = 'agent_up') AS agent_last_seen,
            (SELECT m.value FROM metrics m WHERE m.site_id = s.id AND m.metric = 'agent_spool_depth' ORDER BY m.ts DESC LIMIT 1) AS spool_depth
          FROM sites s ORDER BY s.name",
    )
    .await?;
    let nodes = json_of(
        &mut tx,
        "SELECT id, site_id, kind::text AS kind, name, model, mgmt_ip::text AS mgmt_ip, firmware, distance_m, last_seen_at FROM nodes ORDER BY name",
    )
    .await?;
    let tokens = json_of(&mut tx, "SELECT id, site_id, name, created_at, last_used_at, revoked_at FROM node_tokens ORDER BY created_at DESC").await?;
    tx.commit().await?;
    Ok(json!({ "sites": sites, "nodes": nodes, "node_tokens": tokens }))
}

#[derive(Debug, Deserialize)]
pub struct NewToken {
    pub site: String,
    pub name: String,
}

/// The token is returned once; only its SHA-256 is kept.
pub async fn create_node_token(st: &AppState, tenant: &str, h: &HeaderMap, req: NewToken) -> Result<Value> {
    let a = authenticate(st, tenant, h, Role::Owner).await?;
    let token = new_token("nt");
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let site: Uuid = sqlx::query_scalar("SELECT id FROM sites WHERE slug = $1").bind(&req.site).fetch_optional(&mut *tx).await?.ok_or(AppError::NotFound)?;
    let id: Uuid = sqlx::query_scalar("INSERT INTO node_tokens (tenant_id, site_id, name, token_hash) VALUES ($1,$2,$3,$4) RETURNING id")
        .bind(a.tenant)
        .bind(site)
        .bind(req.name.trim().chars().take(80).collect::<String>())
        .bind(sha256_hex(&token))
        .fetch_one(&mut *tx)
        .await?;
    audit(&mut tx, a.tenant, "admin", Some(&a.actor()), "node_token.created", Some(("node_token", &id.to_string())), json!({ "site": req.site })).await?;
    tx.commit().await?;
    Ok(json!({ "id": id, "token": token, "note": "Copy this now. It cannot be shown again." }))
}

pub async fn revoke_node_token(st: &AppState, tenant: &str, h: &HeaderMap, id: Uuid) -> Result<Value> {
    let a = authenticate(st, tenant, h, Role::Owner).await?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let n = sqlx::query("UPDATE node_tokens SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL").bind(id).execute(&mut *tx).await?.rows_affected();
    if n == 0 {
        return Err(AppError::NotFound);
    }
    audit(&mut tx, a.tenant, "admin", Some(&a.actor()), "node_token.revoked", Some(("node_token", &id.to_string())), json!({})).await?;
    tx.commit().await?;
    Ok(json!({ "ok": true }))
}

#[derive(Debug, Deserialize)]
pub struct MetricsIn {
    pub metrics: Vec<MetricIn>,
}
#[derive(Debug, Deserialize)]
pub struct MetricIn {
    pub metric: String,
    pub value: f64,
}

/// The site agent's health numbers. Authenticated by node token.
pub async fn node_metrics(st: &AppState, headers: &HeaderMap, req: MetricsIn) -> Result<Value> {
    let node = crate::radius::authenticate_node(st, headers).await?;
    if req.metrics.len() > 100 {
        return Err(AppError::BadRequest("at most 100 metrics per request".into()));
    }
    let mut tx = tenant_tx(&st.pool, node.tenant).await?;
    let mut n = 0;
    for m in req
        .metrics
        .iter()
        .filter(|m| m.value.is_finite() && !m.metric.is_empty() && m.metric.len() <= 64 && m.metric.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
    {
        sqlx::query("INSERT INTO metrics (ts, tenant_id, site_id, metric, value) VALUES (now(), $1, $2, $3, $4)")
            .bind(node.tenant)
            .bind(node.site_id)
            .bind(&m.metric)
            .bind(m.value)
            .execute(&mut *tx)
            .await?;
        n += 1;
    }
    tx.commit().await?;
    Ok(json!({ "stored": n }))
}

// ---------------------------------------------------------------------------
// Security, audit, secrets
// ---------------------------------------------------------------------------

pub async fn security(st: &AppState, tenant: &str, h: &HeaderMap) -> Result<Value> {
    let a = authenticate(st, tenant, h, Role::Viewer).await?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let flags = json_of(
        &mut tx,
        "SELECT f.ts, f.signal, f.score, f.action, f.detail, d.mac::text AS mac, d.os_family, d.hostname
        FROM tethering_flags f JOIN devices d ON d.id = f.device_id ORDER BY f.ts DESC LIMIT 100",
    )
    .await?;
    let failures = json_of(
        &mut tx,
        "SELECT action, count(*) AS count_24h FROM audit_log
        WHERE ts > now() - interval '24 hours' AND action IN ('member.login_failed','admin.login_failed','auth.code_rejected','payment.webhook_rejected')
        GROUP BY action",
    )
    .await?;
    let refunds =
        json_of(&mut tx, "SELECT reference, amount_kobo, phone, created_at FROM orders WHERE status = 'refund_flagged' ORDER BY created_at DESC LIMIT 50")
            .await?;
    tx.commit().await?;
    Ok(json!({ "tether_flags": flags, "failures_24h": failures, "refund_flagged_orders": refunds }))
}

pub async fn audit_list(st: &AppState, tenant: &str, h: &HeaderMap, action: Option<&str>, before: Option<i64>, limit: Option<i64>) -> Result<Value> {
    let a = authenticate(st, tenant, h, Role::Viewer).await?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let v: Value = sqlx::query_scalar(
        "SELECT COALESCE(jsonb_agg(t), '[]'::jsonb) FROM (
           SELECT seq, ts, actor_kind, actor_id, action, entity, entity_id, ip::text AS ip, detail
             FROM audit_log WHERE ($1::text IS NULL OR action LIKE $1 || '%') AND ($2::bigint IS NULL OR seq < $2)
            ORDER BY seq DESC LIMIT $3) t",
    )
    .bind(action)
    .bind(before)
    .bind(limit.unwrap_or(100).clamp(1, 500))
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(v)
}

pub async fn audit_verify(st: &AppState, tenant: &str, h: &HeaderMap) -> Result<Value> {
    let a = authenticate(st, tenant, h, Role::Viewer).await?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let broken = json_of(&mut tx, &format!("SELECT bad_seq, problem FROM verify_audit_chain('{}')", a.tenant)).await?;
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log").fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(json!({ "entries": total, "intact": broken.as_array().is_none_or(|b| b.is_empty()), "problems": broken }))
}

/// Spreadsheet programs run text that starts with = + - @ as a formula.
/// Neutralise it, and quote every field.
pub fn csv_field(s: &str) -> String {
    let safe = if s.starts_with(['=', '+', '-', '@', '\t', '\r']) { format!("'{s}") } else { s.to_string() };
    format!("\"{}\"", safe.replace('"', "\"\"").replace(['\n', '\r'], " "))
}

pub async fn audit_csv(st: &AppState, tenant: &str, h: &HeaderMap, from_seq: Option<i64>) -> Result<Response> {
    let a = authenticate(st, tenant, h, Role::Viewer).await?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let rows = sqlx::query(
        "SELECT seq, ts::text AS ts, actor_kind, COALESCE(actor_id,'') AS actor_id, action, COALESCE(entity,'') AS entity, COALESCE(entity_id,'') AS entity_id,
                COALESCE(ip::text,'') AS ip, detail::text AS detail, hash
           FROM audit_log WHERE seq >= $1 ORDER BY seq LIMIT 50000",
    )
    .bind(from_seq.unwrap_or(0))
    .fetch_all(&mut *tx)
    .await?;
    audit(&mut tx, a.tenant, "admin", Some(&a.actor()), "audit.exported", None, json!({ "rows": rows.len() })).await?;
    tx.commit().await?;
    let mut out = String::from("seq,ts,actor_kind,actor_id,action,entity,entity_id,ip,detail,hash\n");
    for r in &rows {
        let cols: Vec<String> = ["ts", "actor_kind", "actor_id", "action", "entity", "entity_id", "ip", "detail", "hash"]
            .iter()
            .map(|c| csv_field(&r.try_get::<String, _>(*c).unwrap_or_default()))
            .collect();
        out.push_str(&format!("{},{}\n", r.try_get::<i64, _>("seq")?, cols.join(",")));
    }
    Ok(([(header::CONTENT_TYPE, "text/csv; charset=utf-8"), (header::CONTENT_DISPOSITION, "attachment; filename=\"audit.csv\"")], out).into_response())
}

#[derive(Debug, Deserialize)]
pub struct PaystackKey {
    pub secret: String,
}

/// Store the tenant's Paystack secret key, encrypted. Never returned again.
pub async fn set_paystack_key(st: &AppState, tenant: &str, h: &HeaderMap, req: PaystackKey) -> Result<Value> {
    let a = authenticate(st, tenant, h, Role::Owner).await?;
    let s = req.secret.trim();
    if !(s.starts_with("sk_live_") || s.starts_with("sk_test_")) || s.len() < 20 || s.len() > 200 || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(AppError::BadRequest("that does not look like a Paystack secret key (it starts with sk_live_ or sk_test_)".into()));
    }
    let enc = st.master_key.encrypt(s)?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    sqlx::query("INSERT INTO tenant_secrets (tenant_id, paystack_secret_enc) VALUES ($1, $2) ON CONFLICT (tenant_id) DO UPDATE SET paystack_secret_enc = $2, updated_at = now()")
        .bind(a.tenant)
        .bind(enc)
        .execute(&mut *tx)
        .await?;
    audit(
        &mut tx,
        a.tenant,
        "admin",
        Some(&a.actor()),
        "secrets.paystack_key_set",
        None,
        json!({ "mode": if s.starts_with("sk_live_") { "live" } else { "test" } }),
    )
    .await?;
    tx.commit().await?;
    Ok(json!({ "ok": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_cells_cannot_run_as_formulas_or_break_rows() {
        assert_eq!(csv_field("=HYPERLINK(\"x\")"), "\"'=HYPERLINK(\"\"x\"\")\"");
        assert_eq!(csv_field("+1"), "\"'+1\"");
        assert_eq!(csv_field("a\nb"), "\"a b\"");
        assert_eq!(csv_field("plain"), "\"plain\"");
    }

    #[test]
    fn roles_are_ordered() {
        assert!(Role::Owner > Role::Operator && Role::Operator > Role::Viewer);
        assert_eq!(role_from("owner"), Role::Owner);
        assert_eq!(role_from("anything"), Role::Viewer);
    }

    #[test]
    fn ranges_are_a_closed_set() {
        assert_eq!(range_of(Some("24h")).unwrap().1, "hour");
        assert!(range_of(Some("1 year'; drop table x;--")).is_err());
        assert_eq!(range_of(None).unwrap().0, "7 days");
    }
}
