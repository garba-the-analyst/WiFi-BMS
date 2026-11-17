//! Member accounts: sign-up, sign-in (password or SMS code), sessions,
//! wallet, devices and connecting.
//!
//! Security notes
//! * Passwords are Argon2id. A wrong phone number costs the same time as a
//!   wrong password (`dummy_verify`), so sign-in does not reveal who is a member.
//! * 5 wrong passwords lock the account for 15 minutes. Attempts are also
//!   rate limited per address and per phone.
//! * SMS codes: 6 digits, 10 minutes, 5 tries, stored hashed, single use.
//! * Session tokens (`mt_...`) are random, shown once, stored as SHA-256.
//! * Money moves only through `wallet_ledger` (append-only). Spending locks the
//!   account row so two purchases cannot spend the same balance.

use axum::http::HeaderMap;
use entitlement::DeviceSlots;
use rand::Rng;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use crate::auth::{bearer, check_password_policy, ct_eq, dummy_verify, hash_password, new_token, sha256_hex, verify_password};
use crate::codes::{new_order_reference, normalise_code};
use crate::db::{audit, load_plan, tenant_id_by_slug, tenant_tx};
use crate::entitlements_db::{grant, load_for_update, now_secs, save, Subject};
use crate::error::{AppError, Result};
use crate::orders::{normalise_phone, payer_email};
use crate::paystack::InitializeRequest;
use crate::radius::{create_ticket, ConnectRequest, Ticket};
use crate::AppState;

pub const SESSION_DAYS: i64 = 30;
const MAX_FAILED: i32 = 5;
const LOCK_MINUTES: i64 = 15;
const OTP_TTL_MINUTES: i64 = 10;
const OTP_MAX_TRIES: i32 = 5;
pub const MIN_TOPUP_KOBO: i64 = 10_000; // N100
pub const MAX_TOPUP_KOBO: i64 = 50_000_000; // N500,000

fn ip_of(st: &AppState, headers: &HeaderMap) -> Option<String> {
    crate::auth::client_ip(headers, st.config.trust_proxy)
}

fn ua_of(headers: &HeaderMap) -> Option<String> {
    headers.get("user-agent").and_then(|v| v.to_str().ok()).map(|s| s.chars().take(400).collect())
}

fn limit(st: &AppState, key: String, max: u32, secs: u64) -> Result<()> {
    st.limiter.hit(&key, max, std::time::Duration::from_secs(secs))
}

/// Create a session row and return the token (shown once).
async fn issue_session(conn: &mut PgConnection, tenant: Uuid, account: Uuid, ip: Option<&str>, ua: Option<&str>) -> Result<String> {
    let token = new_token("mt");
    sqlx::query(
        "INSERT INTO member_sessions (tenant_id, account_id, token_hash, expires_at, ip, user_agent)
         VALUES ($1, $2, $3, now() + make_interval(days => $4), $5::inet, $6)",
    )
    .bind(tenant)
    .bind(account)
    .bind(sha256_hex(&token))
    .bind(SESSION_DAYS as i32)
    .bind(ip)
    .bind(ua)
    .execute(conn)
    .await?;
    Ok(token)
}

/// Who is calling? Returns the tenant and the account behind the bearer token.
pub async fn authenticate(st: &AppState, tenant_slug: &str, headers: &HeaderMap) -> Result<(Uuid, Uuid)> {
    let token = bearer(headers).ok_or(AppError::Unauthorised)?;
    if !token.starts_with("mt_") {
        return Err(AppError::Unauthorised);
    }
    let tenant = tenant_id_by_slug(&st.pool, tenant_slug).await?;
    let mut tx = tenant_tx(&st.pool, tenant).await?;
    let acct: Option<Uuid> = sqlx::query_scalar(
        "UPDATE member_sessions s SET last_used_at = now()
          FROM accounts a
         WHERE s.token_hash = $1 AND s.revoked_at IS NULL AND s.expires_at > now()
           AND a.id = s.account_id AND a.status = 'active'
        RETURNING s.account_id",
    )
    .bind(sha256_hex(token))
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((tenant, acct.ok_or(AppError::Unauthorised)?))
}

// ---------------------------------------------------------------------------
// Register and sign in
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct Register {
    pub phone: String,
    pub password: String,
    #[serde(default)]
    pub display_name: Option<String>,
}

pub async fn register(st: &AppState, tenant_slug: &str, headers: &HeaderMap, req: Register) -> Result<Value> {
    let ip = ip_of(st, headers);
    limit(st, format!("reg-ip:{}", ip.as_deref().unwrap_or("?")), 10, 3600)?;
    let phone = normalise_phone(&req.phone).ok_or_else(|| AppError::BadRequest("enter a valid phone number".into()))?;
    check_password_policy(&req.password, &phone)?;
    let name = req.display_name.map(|n| n.trim().chars().take(80).collect::<String>()).filter(|n| !n.is_empty());
    let hash = hash_password(&req.password)?;

    let tenant = tenant_id_by_slug(&st.pool, tenant_slug).await?;
    let mut tx = tenant_tx(&st.pool, tenant).await?;
    let id: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO accounts (tenant_id, phone, display_name, password_hash) VALUES ($1, $2, $3, $4)
         ON CONFLICT (tenant_id, phone) DO NOTHING RETURNING id",
    )
    .bind(tenant)
    .bind(&phone)
    .bind(name)
    .bind(hash)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(id) = id else {
        return Err(AppError::BadRequest("an account with this phone number already exists. Sign in instead".into()));
    };
    let token = issue_session(&mut tx, tenant, id, ip.as_deref(), ua_of(headers).as_deref()).await?;
    audit(&mut tx, tenant, "account", Some(&id.to_string()), "member.registered", Some(("account", &id.to_string())), json!({})).await?;
    tx.commit().await?;
    Ok(json!({ "token": token, "account_id": id }))
}

#[derive(Debug, Deserialize)]
pub struct Login {
    pub phone: String,
    pub password: String,
}

enum LoginOutcome {
    Ok(Uuid),
    Bad,
    Locked,
}

pub async fn login(st: &AppState, tenant_slug: &str, headers: &HeaderMap, req: Login) -> Result<Value> {
    let ip = ip_of(st, headers);
    limit(st, format!("login-ip:{}", ip.as_deref().unwrap_or("?")), 30, 600)?;
    let phone = normalise_phone(&req.phone).ok_or(AppError::Unauthorised)?;
    limit(st, format!("login-phone:{tenant_slug}:{phone}"), 10, 600)?;
    if req.password.len() > 256 {
        return Err(AppError::Unauthorised);
    }
    let tenant = tenant_id_by_slug(&st.pool, tenant_slug).await?;
    let mut tx = tenant_tx(&st.pool, tenant).await?;
    let row = sqlx::query(
        "SELECT id, password_hash, status::text AS status, (locked_until IS NOT NULL AND locked_until > now()) AS locked
           FROM accounts WHERE phone = $1 FOR UPDATE",
    )
    .bind(&phone)
    .fetch_optional(&mut *tx)
    .await?;

    let outcome = match row {
        None => {
            dummy_verify(&req.password);
            LoginOutcome::Bad
        }
        Some(r) => {
            let id: Uuid = r.try_get("id")?;
            let locked: bool = r.try_get("locked")?;
            let hash: Option<String> = r.try_get("password_hash")?;
            let active = r.try_get::<String, _>("status")? == "active";
            match hash {
                _ if locked => LoginOutcome::Locked,
                None => {
                    dummy_verify(&req.password);
                    LoginOutcome::Bad
                }
                Some(h) if verify_password(&h, &req.password) && active => {
                    sqlx::query("UPDATE accounts SET failed_logins = 0, locked_until = NULL, last_login_at = now() WHERE id = $1")
                        .bind(id)
                        .execute(&mut *tx)
                        .await?;
                    LoginOutcome::Ok(id)
                }
                Some(_) => {
                    sqlx::query(
                        "UPDATE accounts SET failed_logins = CASE WHEN failed_logins + 1 >= $2 THEN 0 ELSE failed_logins + 1 END,
                                locked_until = CASE WHEN failed_logins + 1 >= $2 THEN now() + make_interval(mins => $3) ELSE locked_until END
                          WHERE id = $1",
                    )
                    .bind(id)
                    .bind(MAX_FAILED)
                    .bind(LOCK_MINUTES as i32)
                    .execute(&mut *tx)
                    .await?;
                    audit(&mut tx, tenant, "account", Some(&id.to_string()), "member.login_failed", Some(("account", &id.to_string())), json!({})).await?;
                    LoginOutcome::Bad
                }
            }
        }
    };
    match outcome {
        LoginOutcome::Ok(id) => {
            let token = issue_session(&mut tx, tenant, id, ip.as_deref(), ua_of(headers).as_deref()).await?;
            audit(&mut tx, tenant, "account", Some(&id.to_string()), "member.login", Some(("account", &id.to_string())), json!({})).await?;
            tx.commit().await?;
            Ok(json!({ "token": token, "account_id": id }))
        }
        LoginOutcome::Bad => {
            tx.commit().await?; // keep the failure count
            Err(AppError::Unauthorised)
        }
        LoginOutcome::Locked => {
            tx.commit().await?;
            Err(AppError::RateLimited)
        }
    }
}

pub async fn logout(st: &AppState, tenant_slug: &str, headers: &HeaderMap) -> Result<Value> {
    let (tenant, _) = authenticate(st, tenant_slug, headers).await?;
    let token = bearer(headers).ok_or(AppError::Unauthorised)?;
    let mut tx = tenant_tx(&st.pool, tenant).await?;
    sqlx::query("UPDATE member_sessions SET revoked_at = now() WHERE token_hash = $1").bind(sha256_hex(token)).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(json!({ "ok": true }))
}

// ---------------------------------------------------------------------------
// SMS code sign-in
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct OtpRequest {
    pub phone: String,
}

fn otp_hash(phone: &str, code: &str) -> String {
    sha256_hex(&format!("otp:{phone}:{code}"))
}

/// Always answers the same way, so it does not reveal whether a number is registered.
pub async fn otp_request(st: &AppState, tenant_slug: &str, headers: &HeaderMap, req: OtpRequest) -> Result<Value> {
    let ip = ip_of(st, headers);
    limit(st, format!("otp-ip:{}", ip.as_deref().unwrap_or("?")), 20, 3600)?;
    let phone = normalise_phone(&req.phone).ok_or_else(|| AppError::BadRequest("enter a valid phone number".into()))?;
    limit(st, format!("otp-phone:{tenant_slug}:{phone}"), 3, 600)?;
    let tenant = tenant_id_by_slug(&st.pool, tenant_slug).await?;
    let code = format!("{:06}", rand::thread_rng().gen_range(0..1_000_000u32));
    let mut tx = tenant_tx(&st.pool, tenant).await?;
    sqlx::query(
        "INSERT INTO otp_codes (tenant_id, phone, purpose, code_hash, expires_at)
         VALUES ($1, $2, 'login', $3, now() + make_interval(mins => $4))",
    )
    .bind(tenant)
    .bind(&phone)
    .bind(otp_hash(&phone, &code))
    .bind(OTP_TTL_MINUTES as i32)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    st.sms.send(&phone, &format!("Your sign-in code is {code}. It expires in {OTP_TTL_MINUTES} minutes. Never share it.")).await?;
    Ok(json!({ "sent": true }))
}

#[derive(Debug, Deserialize)]
pub struct OtpVerify {
    pub phone: String,
    pub code: String,
}

/// Verify a code. A number with no account gets one (SMS-only member).
pub async fn otp_verify(st: &AppState, tenant_slug: &str, headers: &HeaderMap, req: OtpVerify) -> Result<Value> {
    let ip = ip_of(st, headers);
    limit(st, format!("otpv-ip:{}", ip.as_deref().unwrap_or("?")), 40, 600)?;
    let phone = normalise_phone(&req.phone).ok_or(AppError::Unauthorised)?;
    let code: String = req.code.chars().filter(char::is_ascii_digit).collect();
    let tenant = tenant_id_by_slug(&st.pool, tenant_slug).await?;
    let mut tx = tenant_tx(&st.pool, tenant).await?;

    let row = sqlx::query(
        "SELECT id, code_hash, attempts FROM otp_codes
          WHERE phone = $1 AND purpose = 'login' AND consumed_at IS NULL AND expires_at > now()
          ORDER BY created_at DESC LIMIT 1 FOR UPDATE",
    )
    .bind(&phone)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        tx.commit().await?;
        return Err(AppError::Unauthorised);
    };
    let (otp_id, hash, attempts): (Uuid, String, i32) = (row.try_get("id")?, row.try_get("code_hash")?, row.try_get("attempts")?);
    if attempts >= OTP_MAX_TRIES {
        tx.commit().await?;
        return Err(AppError::Unauthorised);
    }
    sqlx::query("UPDATE otp_codes SET attempts = attempts + 1 WHERE id = $1").bind(otp_id).execute(&mut *tx).await?;
    if !ct_eq(&hash, &otp_hash(&phone, &code)) {
        tx.commit().await?; // keep the attempt count
        return Err(AppError::Unauthorised);
    }
    sqlx::query("UPDATE otp_codes SET consumed_at = now() WHERE id = $1").bind(otp_id).execute(&mut *tx).await?;

    let acct: Uuid = sqlx::query_scalar(
        "INSERT INTO accounts (tenant_id, phone) VALUES ($1, $2)
         ON CONFLICT (tenant_id, phone) DO UPDATE SET last_login_at = now() RETURNING id",
    )
    .bind(tenant)
    .bind(&phone)
    .fetch_one(&mut *tx)
    .await?;
    let status: String = sqlx::query_scalar("SELECT status::text FROM accounts WHERE id = $1").bind(acct).fetch_one(&mut *tx).await?;
    if status != "active" {
        tx.commit().await?;
        return Err(AppError::Forbidden);
    }
    let token = issue_session(&mut tx, tenant, acct, ip.as_deref(), ua_of(headers).as_deref()).await?;
    audit(&mut tx, tenant, "account", Some(&acct.to_string()), "member.login_sms", Some(("account", &acct.to_string())), json!({})).await?;
    tx.commit().await?;
    Ok(json!({ "token": token, "account_id": acct }))
}

// ---------------------------------------------------------------------------
// Profile, wallet
// ---------------------------------------------------------------------------

async fn balance(conn: &mut PgConnection, account: Uuid) -> Result<i64> {
    Ok(sqlx::query_scalar("SELECT COALESCE(sum(amount_kobo), 0)::bigint FROM wallet_ledger WHERE account_id = $1").bind(account).fetch_one(conn).await?)
}

pub async fn me(st: &AppState, tenant_slug: &str, headers: &HeaderMap) -> Result<Value> {
    let (tenant, acct) = authenticate(st, tenant_slug, headers).await?;
    let mut tx = tenant_tx(&st.pool, tenant).await?;
    let a = sqlx::query("SELECT phone, display_name, email FROM accounts WHERE id = $1").bind(acct).fetch_one(&mut *tx).await?;
    let bal = balance(&mut tx, acct).await?;
    let ents = sqlx::query(
        "SELECT e.id, p.name AS plan, e.status::text AS status,
                extract(epoch FROM e.expires_at)::bigint AS expires,
                e.remaining_active_seconds AS secs, e.remaining_bytes AS bytes
           FROM entitlements e JOIN plans p ON p.id = e.plan_id
          WHERE e.account_id = $1 ORDER BY e.granted_at DESC LIMIT 50",
    )
    .bind(acct)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let mut list = Vec::new();
    for r in &ents {
        list.push(json!({
            "id": r.try_get::<Uuid, _>("id")?, "plan": r.try_get::<String, _>("plan")?, "status": r.try_get::<String, _>("status")?,
            "expires_at": r.try_get::<i64, _>("expires")?,
            "remaining_active_seconds": r.try_get::<Option<i32>, _>("secs")?, "remaining_bytes": r.try_get::<Option<i64>, _>("bytes")?,
        }));
    }
    Ok(json!({
        "phone": a.try_get::<String, _>("phone")?, "display_name": a.try_get::<Option<String>, _>("display_name")?,
        "email": a.try_get::<Option<String>, _>("email")?, "balance_kobo": bal, "entitlements": list,
    }))
}

pub async fn wallet(st: &AppState, tenant_slug: &str, headers: &HeaderMap) -> Result<Value> {
    let (tenant, acct) = authenticate(st, tenant_slug, headers).await?;
    let mut tx = tenant_tx(&st.pool, tenant).await?;
    let bal = balance(&mut tx, acct).await?;
    let rows = sqlx::query(
        "SELECT kind::text AS kind, amount_kobo, note, extract(epoch FROM ts)::bigint AS ts
           FROM wallet_ledger WHERE account_id = $1 ORDER BY id DESC LIMIT 50",
    )
    .bind(acct)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let mut entries = Vec::new();
    for r in &rows {
        entries.push(json!({ "kind": r.try_get::<String, _>("kind")?, "amount_kobo": r.try_get::<i64, _>("amount_kobo")?,
            "note": r.try_get::<Option<String>, _>("note")?, "ts": r.try_get::<i64, _>("ts")? }));
    }
    Ok(json!({ "balance_kobo": bal, "entries": entries }))
}

#[derive(Debug, Deserialize)]
pub struct TopUp {
    pub site: String,
    pub amount_kobo: i64,
}

/// Start a Paystack payment that credits the wallet once verified (see `fulfil`).
pub async fn topup(st: &AppState, tenant_slug: &str, headers: &HeaderMap, req: TopUp) -> Result<Value> {
    let (tenant, acct) = authenticate(st, tenant_slug, headers).await?;
    if !(MIN_TOPUP_KOBO..=MAX_TOPUP_KOBO).contains(&req.amount_kobo) {
        return Err(AppError::BadRequest("top-up must be between N100 and N500,000".into()));
    }
    limit(st, format!("topup:{acct}"), 10, 3600)?;
    let reference = new_order_reference();
    let mut tx = tenant_tx(&st.pool, tenant).await?;
    let site_id: Uuid = sqlx::query_scalar("SELECT id FROM sites WHERE slug = $1 AND status = 'active'")
        .bind(&req.site)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(AppError::NotFound)?;
    let acc = sqlx::query("SELECT phone, email FROM accounts WHERE id = $1").bind(acct).fetch_one(&mut *tx).await?;
    let (phone, email): (String, Option<String>) = (acc.try_get("phone")?, acc.try_get("email")?);
    let order_id: Uuid = sqlx::query_scalar(
        "INSERT INTO orders (tenant_id, site_id, plan_id, reference, amount_kobo, account_id, phone, purpose, status)
         VALUES ($1, $2, NULL, $3, $4, $5, $6, 'topup', 'created') RETURNING id",
    )
    .bind(tenant)
    .bind(site_id)
    .bind(&reference)
    .bind(req.amount_kobo)
    .bind(acct)
    .bind(&phone)
    .fetch_one(&mut *tx)
    .await?;
    let t = sqlx::query("SELECT payment_mode, paystack_subaccount FROM tenants WHERE id = $1").bind(tenant).fetch_one(&mut *tx).await?;
    let mode: String = t.try_get("payment_mode")?;
    let sub: Option<String> = t.try_get("paystack_subaccount")?;
    let secret = st.load_paystack_secret(&mut tx, tenant).await?;
    audit(
        &mut tx,
        tenant,
        "account",
        Some(&acct.to_string()),
        "wallet.topup_started",
        Some(("order", &order_id.to_string())),
        json!({ "amount_kobo": req.amount_kobo }),
    )
    .await?;
    tx.commit().await?;

    let em = payer_email(email.as_deref(), &phone, &st.config.fallback_email_domain);
    let callback = format!("{}/t/{}/wallet?ref={}", st.config.public_base_url, tenant_slug, reference);
    let init = st
        .gateway
        .initialize(InitializeRequest {
            secret: &secret,
            reference: &reference,
            amount_kobo: req.amount_kobo,
            currency: "NGN",
            email: &em,
            callback_url: Some(&callback),
            subaccount: if mode == "subaccount" { sub.as_deref() } else { None },
            metadata: json!({ "order_id": order_id, "purpose": "topup" }),
        })
        .await;
    let mut tx = tenant_tx(&st.pool, tenant).await?;
    let status = if init.is_ok() { "pending" } else { "failed" };
    sqlx::query("UPDATE orders SET status = $2::order_status WHERE id = $1 AND status = 'created'").bind(order_id).bind(status).execute(&mut *tx).await?;
    tx.commit().await?;
    let i = init?;
    Ok(json!({ "reference": reference, "authorization_url": i.authorization_url, "amount_kobo": req.amount_kobo }))
}

#[derive(Debug, Deserialize)]
pub struct BuyWithWallet {
    pub site: String,
    pub plan_id: Uuid,
}

/// Buy a plan from the wallet balance. One transaction: lock, check, debit, grant.
pub async fn buy_with_wallet(st: &AppState, tenant_slug: &str, headers: &HeaderMap, req: BuyWithWallet) -> Result<Value> {
    let (tenant, acct) = authenticate(st, tenant_slug, headers).await?;
    let mut tx = tenant_tx(&st.pool, tenant).await?;
    // Serialises this member's spending.
    let phone: String = sqlx::query_scalar("SELECT phone FROM accounts WHERE id = $1 FOR UPDATE").bind(acct).fetch_one(&mut *tx).await?;
    let site_id: Uuid = sqlx::query_scalar("SELECT id FROM sites WHERE slug = $1 AND status = 'active'")
        .bind(&req.site)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(AppError::NotFound)?;
    let active: Option<bool> = sqlx::query_scalar("SELECT is_active FROM plans WHERE id = $1").bind(req.plan_id).fetch_optional(&mut *tx).await?;
    if active != Some(true) {
        return Err(AppError::NotFound);
    }
    let plan = load_plan(&mut tx, req.plan_id).await?;
    if plan.price_kobo <= 0 {
        return Err(AppError::BadRequest("this plan cannot be bought from the wallet".into()));
    }
    let bal = balance(&mut tx, acct).await?;
    if bal < plan.price_kobo {
        return Err(AppError::BadRequest(format!("not enough balance: you have {bal} kobo and this plan costs {}", plan.price_kobo)));
    }
    let reference = new_order_reference();
    let order_id: Uuid = sqlx::query_scalar(
        "INSERT INTO orders (tenant_id, site_id, plan_id, reference, amount_kobo, account_id, phone, paid_with, status, paid_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'wallet', 'provisioned', now()) RETURNING id",
    )
    .bind(tenant)
    .bind(site_id)
    .bind(req.plan_id)
    .bind(&reference)
    .bind(plan.price_kobo)
    .bind(acct)
    .bind(&phone)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO wallet_ledger (tenant_id, account_id, kind, amount_kobo, order_id, note) VALUES ($1, $2, 'purchase', $3, $4, $5)")
        .bind(tenant)
        .bind(acct)
        .bind(-plan.price_kobo)
        .bind(order_id)
        .bind(format!("Plan: {}", plan.name))
        .execute(&mut *tx)
        .await?;
    let ent = grant(&mut tx, tenant, order_id, req.plan_id, &plan.plan, Subject::Account(acct)).await?;
    audit(
        &mut tx,
        tenant,
        "account",
        Some(&acct.to_string()),
        "wallet.purchase",
        Some(("order", &order_id.to_string())),
        json!({ "plan": plan.name, "amount_kobo": plan.price_kobo, "entitlement": ent }),
    )
    .await?;
    tx.commit().await?;
    Ok(json!({ "reference": reference, "entitlement_id": ent, "balance_kobo": bal - plan.price_kobo }))
}

// ---------------------------------------------------------------------------
// Claim a guest code, connect, devices
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct Claim {
    pub code: String,
}

/// Move a guest code's purchases into the member's account. Possession of the
/// code is the proof; guessing is rate limited.
pub async fn claim_code(st: &AppState, tenant_slug: &str, headers: &HeaderMap, req: Claim) -> Result<Value> {
    let (tenant, acct) = authenticate(st, tenant_slug, headers).await?;
    limit(st, format!("claim:{acct}"), 10, 600)?;
    let code = normalise_code(&req.code);
    let mut tx = tenant_tx(&st.pool, tenant).await?;
    let row = sqlx::query("SELECT id, claimed_by_account_id FROM access_codes WHERE code = $1 FOR UPDATE").bind(&code).fetch_optional(&mut *tx).await?;
    let Some(row) = row else {
        return Err(AppError::BadRequest("that code was not recognised".into()));
    };
    let (code_id, claimed): (Uuid, Option<Uuid>) = (row.try_get("id")?, row.try_get("claimed_by_account_id")?);
    if claimed.is_some() {
        return Err(AppError::BadRequest("that code has already been added to an account".into()));
    }
    let moved = sqlx::query("UPDATE entitlements SET account_id = $1, access_code_id = NULL WHERE access_code_id = $2")
        .bind(acct)
        .bind(code_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    sqlx::query("UPDATE access_codes SET claimed_by_account_id = $1 WHERE id = $2").bind(acct).bind(code_id).execute(&mut *tx).await?;
    audit(
        &mut tx,
        tenant,
        "account",
        Some(&acct.to_string()),
        "member.code_claimed",
        Some(("access_code", &code_id.to_string())),
        json!({ "entitlements": moved }),
    )
    .await?;
    tx.commit().await?;
    Ok(json!({ "moved": moved }))
}

#[derive(Debug, Deserialize)]
pub struct MemberConnect {
    pub site: String,
    pub mac: String,
    #[serde(default)]
    pub accept_terms: bool,
    #[serde(default)]
    pub dhcp_hostname: Option<String>,
}

pub async fn connect(st: &AppState, tenant_slug: &str, headers: &HeaderMap, req: MemberConnect) -> Result<Ticket> {
    let (tenant, acct) = authenticate(st, tenant_slug, headers).await?;
    let ip = ip_of(st, headers);
    let cr = ConnectRequest { site: req.site, mac: req.mac, code: None, accept_terms: req.accept_terms, dhcp_hostname: req.dhcp_hostname };
    let mut tx = tenant_tx(&st.pool, tenant).await?;
    let t = create_ticket(&mut tx, tenant, &cr, Subject::Account(acct), headers, ip.as_deref()).await?;
    tx.commit().await?;
    Ok(t)
}

pub async fn devices(st: &AppState, tenant_slug: &str, headers: &HeaderMap) -> Result<Value> {
    let (tenant, acct) = authenticate(st, tenant_slug, headers).await?;
    let mut tx = tenant_tx(&st.pool, tenant).await?;
    let rows = sqlx::query(
        "SELECT b.entitlement_id, d.id AS device_id, d.mac::text AS mac, d.hostname, d.os_family, d.device_class,
                extract(epoch FROM b.bound_at)::bigint AS bound_at
           FROM device_bindings b
           JOIN entitlements e ON e.id = b.entitlement_id
           JOIN devices d ON d.id = b.device_id
          WHERE e.account_id = $1 AND b.released_at IS NULL AND e.status IN ('pending', 'active')
          ORDER BY b.bound_at",
    )
    .bind(acct)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let mut out = Vec::new();
    for r in &rows {
        out.push(json!({
            "entitlement_id": r.try_get::<Uuid, _>("entitlement_id")?, "device_id": r.try_get::<Uuid, _>("device_id")?,
            "mac": r.try_get::<String, _>("mac")?, "hostname": r.try_get::<Option<String>, _>("hostname")?,
            "os": r.try_get::<Option<String>, _>("os_family")?, "class": r.try_get::<Option<String>, _>("device_class")?,
            "bound_at": r.try_get::<i64, _>("bound_at")?,
        }));
    }
    Ok(json!({ "devices": out }))
}

#[derive(Debug, Deserialize)]
pub struct Swap {
    pub entitlement_id: Uuid,
    pub old_device_id: Uuid,
    /// MAC of the new device. It must already have asked to connect with this account.
    pub new_mac: String,
}

/// Replace one device on a plan with another, honouring the plan's cooldown.
pub async fn swap_device(st: &AppState, tenant_slug: &str, headers: &HeaderMap, req: Swap) -> Result<Value> {
    let (tenant, acct) = authenticate(st, tenant_slug, headers).await?;
    let mac = crate::radius::normalise_mac(&req.new_mac)?;
    let mut tx = tenant_tx(&st.pool, tenant).await?;

    let mut l = load_for_update(&mut tx, req.entitlement_id).await?;
    let owner: Option<Uuid> = sqlx::query_scalar("SELECT account_id FROM entitlements WHERE id = $1").bind(l.id).fetch_one(&mut *tx).await?;
    if owner != Some(acct) {
        return Err(AppError::NotFound);
    }
    let new_dev: Uuid = sqlx::query_scalar(
        "SELECT d.id FROM devices d
          WHERE d.mac = $1::macaddr
            AND EXISTS (SELECT 1 FROM connect_tickets t WHERE t.account_id = $2 AND t.mac = d.mac)",
    )
    .bind(&mac)
    .bind(acct)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::BadRequest("connect the new device to the Wi-Fi and sign in on it first".into()))?;

    let mut slots = DeviceSlots::new(l.ent.plan.device_limit, l.ent.plan.swap_cooldown_secs);
    slots.last_swap_at = l.last_swap_at;
    let bound = sqlx::query(
        "SELECT device_id::text AS d, extract(epoch FROM bound_at)::bigint AS at FROM device_bindings WHERE entitlement_id = $1 AND released_at IS NULL",
    )
    .bind(l.id)
    .fetch_all(&mut *tx)
    .await?;
    for b in &bound {
        slots.bindings.push(entitlement::slots::Binding { device_id: b.try_get("d")?, bound_at: b.try_get("at")? });
    }
    let now = now_secs();
    slots.swap(&req.old_device_id.to_string(), &new_dev.to_string(), now).map_err(|e| AppError::BadRequest(e.to_string()))?;

    sqlx::query("UPDATE device_bindings SET released_at = now() WHERE entitlement_id = $1 AND device_id = $2 AND released_at IS NULL")
        .bind(l.id)
        .bind(req.old_device_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO device_bindings (tenant_id, entitlement_id, device_id) VALUES ($1, $2, $3)")
        .bind(tenant)
        .bind(l.id)
        .bind(new_dev)
        .execute(&mut *tx)
        .await?;
    l.last_swap_at = slots.last_swap_at;
    save(&mut tx, &l).await?;
    audit(
        &mut tx,
        tenant,
        "account",
        Some(&acct.to_string()),
        "member.device_swapped",
        Some(("entitlement", &l.id.to_string())),
        json!({ "old": req.old_device_id, "new": new_dev }),
    )
    .await?;
    tx.commit().await?;
    Ok(json!({ "ok": true, "next_swap_at": now + i64::from(slots.swap_cooldown_secs) }))
}
