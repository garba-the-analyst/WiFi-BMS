//! Staff sign-in and accounts: password + 2-step verification (authenticator app, email code or
//! text-message code), enrolment, and managing other staff.
//!
//! Sign-in is two requests:
//!
//! 1. `login {email, password}`. A staff member who has set up 2-step verification gets a
//!    `challenge` id back (and, for email/SMS, a code is sent). No token yet.
//! 2. `login/verify {challenge, code}` gives the session token.
//!
//! A member who has NOT yet set up 2-step verification gets a token that can only be used to
//! set it up (see [`crate::admin::authenticate`]); nothing else works until they do.
//!
//! Codes are 6 digits, valid 10 minutes, five wrong tries and the challenge is dead. They are
//! stored only as a salted hash.

use axum::http::HeaderMap;
use rand::Rng;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use crate::admin::{allows, authenticate, ip_of, AdminCtx, Perm, Role, ROLE_NAMES, SESSION_HOURS};
use crate::auth::{check_password_policy, dummy_verify, hash_password, new_token, sha256_hex, verify_password};
use crate::db::{audit, tenant_id_by_slug, tenant_tx};
use crate::errlog::{record, NewEvent};
use crate::error::{AppError, Result};
use crate::totp;
use crate::AppState;

const MAX_FAILED: i32 = 5;
const LOCK_MINUTES: i32 = 15;
const CODE_MINUTES: i32 = 10;
const MAX_CODE_TRIES: i32 = 5;

#[derive(Debug, Deserialize)]
pub struct Login {
    pub email: String,
    pub password: String,
}

fn six_digits() -> String {
    format!("{:06}", rand::thread_rng().gen_range(0..1_000_000))
}

fn code_hash(challenge: Uuid, code: &str) -> String {
    sha256_hex(&format!("{challenge}:{code}"))
}

fn mask_email(e: &str) -> String {
    match e.split_once('@') {
        Some((l, d)) => format!("{}***@{}", l.chars().next().unwrap_or('*'), d),
        None => "***".into(),
    }
}

fn mask_phone(p: &str) -> String {
    let n = p.chars().count();
    if n <= 4 {
        "***".into()
    } else {
        format!("{}***{}", p.chars().take(3).collect::<String>(), p.chars().skip(n - 3).collect::<String>())
    }
}

async fn issue_session(tx: &mut PgConnection, tenant: Uuid, admin: Uuid, ip: Option<&str>, ua: Option<String>) -> Result<String> {
    let token = new_token("at");
    sqlx::query(
        "INSERT INTO admin_sessions (tenant_id, admin_user_id, token_hash, expires_at, ip, user_agent)
         VALUES ($1, $2, $3, now() + make_interval(hours => $4), $5::inet, $6)",
    )
    .bind(tenant)
    .bind(admin)
    .bind(sha256_hex(&token))
    .bind(SESSION_HOURS)
    .bind(ip)
    .bind(ua)
    .execute(tx)
    .await?;
    Ok(token)
}

fn ua_of(h: &HeaderMap) -> Option<String> {
    h.get("user-agent").and_then(|v| v.to_str().ok()).map(|s| s.chars().take(300).collect::<String>())
}

/// Send a code by the chosen channel. A failure is logged to the error log and reported.
async fn deliver(st: &AppState, tenant: Uuid, method: &str, email: &str, phone: Option<&str>, code: &str) -> Result<()> {
    let text = format!(
        "Your sign-in code is {code}. It works for {CODE_MINUTES} minutes. If you did not ask for it, ignore this message and tell your system administrator."
    );
    let r = match method {
        "email" => st.mailer.send(email, "Your Wi-Fi back-office code", &text).await,
        "sms" => st.sms.send(phone.ok_or_else(|| AppError::BadRequest("no phone number on this account".into()))?, &text).await,
        _ => Ok(()),
    };
    if let Err(e) = &r {
        let (src, code_name) = if method == "email" { ("email", "email.send_failed") } else { ("sms", "sms.send_failed") };
        record(&st.pool, tenant, NewEvent::new(src, "error", code_name, format!("{e}")).details(json!({ "purpose": "staff 2-step code" }))).await;
    }
    r
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
        "SELECT id, password_hash, status::text AS status, role, mfa_method, phone,
                (locked_until IS NOT NULL AND locked_until > now()) AS locked
           FROM admin_users WHERE lower(email) = $1 FOR UPDATE",
    )
    .bind(&email)
    .fetch_optional(&mut *tx)
    .await?;

    let Some(r) = row else {
        dummy_verify(&req.password);
        tx.commit().await?;
        return Err(AppError::Unauthorised);
    };
    let id: Uuid = r.try_get("id")?;
    let locked: bool = r.try_get("locked")?;
    let active = r.try_get::<String, _>("status")? == "active";
    if locked {
        tx.commit().await?;
        return Err(AppError::RateLimited);
    }
    if !(verify_password(&r.try_get::<String, _>("password_hash")?, &req.password) && active) {
        fail(&mut tx, tenant, id).await?;
        tx.commit().await?;
        return Err(AppError::Unauthorised);
    }
    let role: String = r.try_get("role")?;
    let method: String = r.try_get("mfa_method")?;
    if method == "none" {
        // Password is right but there is no second step yet: a limited session, only good for setting it up.
        sqlx::query("UPDATE admin_users SET failed_logins = 0, locked_until = NULL, last_login_at = now() WHERE id = $1").bind(id).execute(&mut *tx).await?;
        let token = issue_session(&mut tx, tenant, id, ip.as_deref(), ua_of(headers)).await?;
        audit(&mut tx, tenant, "admin", Some(&id.to_string()), "admin.login", Some(("admin", &id.to_string())), json!({ "mfa": "none" })).await?;
        tx.commit().await?;
        return Ok(json!({ "token": token, "role": role, "mfa_method": "none", "setup_required": st.config.require_2fa, "expires_in_hours": SESSION_HOURS }));
    }
    // Make a challenge. Older unused ones for this person are cancelled so only the newest code works.
    sqlx::query("UPDATE admin_challenges SET used_at = now() WHERE admin_id = $1 AND purpose = 'login' AND used_at IS NULL").bind(id).execute(&mut *tx).await?;
    let challenge = Uuid::new_v4();
    let code = if method == "totp" { None } else { Some(six_digits()) };
    sqlx::query(
        "INSERT INTO admin_challenges (id, tenant_id, admin_id, purpose, method, code_hash, expires_at, ip)
         VALUES ($1, $2, $3, 'login', $4, $5, now() + make_interval(mins => $6), $7::inet)",
    )
    .bind(challenge)
    .bind(tenant)
    .bind(id)
    .bind(&method)
    .bind(code.as_deref().map(|c| code_hash(challenge, c)))
    .bind(CODE_MINUTES)
    .bind(ip.as_deref())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    let phone: Option<String> = r.try_get("phone")?;
    let sent_to = match (method.as_str(), code.as_deref()) {
        ("email", Some(c)) => {
            deliver(st, tenant, "email", &email, None, c)
                .await
                .map_err(|_| AppError::BadRequest("we could not send your code by email. Ask a system administrator to check the email settings".into()))?;
            Some(mask_email(&email))
        }
        ("sms", Some(c)) => {
            deliver(st, tenant, "sms", &email, phone.as_deref(), c).await.map_err(|_| {
                AppError::BadRequest("we could not send your code by text message. Ask a system administrator to check the SMS settings".into())
            })?;
            phone.as_deref().map(mask_phone)
        }
        _ => None,
    };
    Ok(json!({ "challenge": challenge, "method": method, "sent_to": sent_to }))
}

async fn fail(tx: &mut PgConnection, tenant: Uuid, id: Uuid) -> Result<()> {
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
    audit(tx, tenant, "admin", Some(&id.to_string()), "admin.login_failed", Some(("admin", &id.to_string())), json!({})).await?;
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct Verify {
    pub challenge: Uuid,
    pub code: String,
}

fn totp_ok(st: &AppState, secret_enc: &str, code: &str) -> Result<bool> {
    let secret = st.master_key.decrypt(secret_enc)?;
    let bytes = totp::base32_decode(&secret).ok_or_else(|| AppError::Internal("totp secret unreadable".into()))?;
    Ok(totp::verify(&bytes, crate::entitlements_db::now_secs() as u64, code).is_some())
}

/// Check a code against a challenge row (already locked). Counts the try.
async fn check_challenge(st: &AppState, tx: &mut PgConnection, ch: &sqlx::postgres::PgRow, admin_secret_enc: Option<&str>, code: &str) -> Result<bool> {
    let id: Uuid = ch.try_get("id")?;
    let method: String = ch.try_get("method")?;
    let attempts: i32 = ch.try_get("attempts")?;
    if attempts >= MAX_CODE_TRIES {
        return Ok(false);
    }
    let code = code.trim();
    let ok = if method == "totp" {
        // Enrolment keeps the pending secret in `target`; sign-in uses the admin's stored secret.
        let target: Option<String> = ch.try_get("target")?;
        match target.as_deref().or(admin_secret_enc) {
            Some(enc) => totp_ok(st, enc, code)?,
            None => false,
        }
    } else {
        let want: Option<String> = ch.try_get("code_hash")?;
        match want {
            Some(w) => subtle::ConstantTimeEq::ct_eq(w.as_bytes(), code_hash(id, code).as_bytes()).into(),
            None => false,
        }
    };
    if ok {
        sqlx::query("UPDATE admin_challenges SET used_at = now() WHERE id = $1").bind(id).execute(tx).await?;
    } else {
        sqlx::query("UPDATE admin_challenges SET attempts = attempts + 1 WHERE id = $1").bind(id).execute(tx).await?;
    }
    Ok(ok)
}

pub async fn login_verify(st: &AppState, tenant_slug: &str, headers: &HeaderMap, req: Verify) -> Result<Value> {
    let ip = ip_of(st, headers);
    st.limiter.hit(&format!("admin-verify-ip:{}", ip.as_deref().unwrap_or("?")), 30, std::time::Duration::from_secs(600))?;
    let tenant = tenant_id_by_slug(&st.pool, tenant_slug).await?;
    let mut tx = tenant_tx(&st.pool, tenant).await?;
    let ch = sqlx::query(
        "SELECT c.id, c.admin_id, c.method, c.code_hash, c.target, c.attempts, u.totp_secret, u.role, u.status::text AS status
           FROM admin_challenges c JOIN admin_users u ON u.id = c.admin_id
          WHERE c.id = $1 AND c.purpose = 'login' AND c.used_at IS NULL AND c.expires_at > now() FOR UPDATE OF c",
    )
    .bind(req.challenge)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(ch) = ch else {
        tx.commit().await?;
        return Err(AppError::Unauthorised);
    };
    let admin: Uuid = ch.try_get("admin_id")?;
    let secret_enc: Option<Vec<u8>> = ch.try_get("totp_secret")?;
    let secret_enc = secret_enc.and_then(|b| String::from_utf8(b).ok());
    let ok = check_challenge(st, &mut tx, &ch, secret_enc.as_deref(), &req.code).await? && ch.try_get::<String, _>("status")? == "active";
    if !ok {
        fail(&mut tx, tenant, admin).await?;
        tx.commit().await?;
        return Err(AppError::Unauthorised);
    }
    sqlx::query("UPDATE admin_users SET failed_logins = 0, locked_until = NULL, last_login_at = now() WHERE id = $1").bind(admin).execute(&mut *tx).await?;
    let token = issue_session(&mut tx, tenant, admin, ip.as_deref(), ua_of(headers)).await?;
    let method: String = ch.try_get("method")?;
    audit(&mut tx, tenant, "admin", Some(&admin.to_string()), "admin.login", Some(("admin", &admin.to_string())), json!({ "mfa": method })).await?;
    tx.commit().await?;
    Ok(json!({ "token": token, "role": ch.try_get::<String, _>("role")?, "mfa_method": method, "expires_in_hours": SESSION_HOURS }))
}

// ---------------------------------------------------------------------------
// Enrolment (every signed-in account, even before 2-step verification is on)
// ---------------------------------------------------------------------------

pub async fn me(st: &AppState, tenant: &str, h: &HeaderMap) -> Result<Value> {
    let a = authenticate(st, tenant, h, Perm::SelfService).await?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let row = sqlx::query("SELECT display_name, phone FROM admin_users WHERE id = $1").bind(a.id).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    let perms: Vec<&str> = Perm::ALL.iter().filter(|(p, _)| allows(a.role, *p)).map(|(_, n)| *n).collect();
    let phone: Option<String> = row.try_get("phone")?;
    Ok(json!({
        "id": a.id, "email": a.email, "role": a.role.as_str(), "display_name": row.try_get::<Option<String>, _>("display_name")?,
        "mfa_method": a.mfa_method, "phone": phone.as_deref().map(mask_phone), "permissions": perms,
        "setup_required": st.config.require_2fa && a.mfa_method == "none",
        "methods_available": { "totp": true, "email": st.mailer.is_enabled(), "sms": st.sms.is_enabled() },
    }))
}

#[derive(Debug, Deserialize)]
pub struct StartMfa {
    pub method: String,
    #[serde(default)]
    pub phone: Option<String>,
}

pub async fn mfa_start(st: &AppState, tenant: &str, h: &HeaderMap, req: StartMfa) -> Result<Value> {
    let a = authenticate(st, tenant, h, Perm::SelfService).await?;
    st.limiter.hit(&format!("mfa-start:{}", a.id), 8, std::time::Duration::from_secs(600))?;
    if !["totp", "email", "sms"].contains(&req.method.as_str()) {
        return Err(AppError::BadRequest("method must be totp, email or sms".into()));
    }
    let challenge = Uuid::new_v4();
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    sqlx::query("UPDATE admin_challenges SET used_at = now() WHERE admin_id = $1 AND purpose = 'enrol' AND used_at IS NULL")
        .bind(a.id)
        .execute(&mut *tx)
        .await?;
    let (code, target, extra): (Option<String>, Option<String>, Value) = match req.method.as_str() {
        "totp" => {
            let raw = totp::new_secret();
            let enc = st.master_key.encrypt(&totp::base32_encode(&raw))?;
            (None, Some(enc), json!({ "otpauth_uri": totp::provisioning_uri("WiFi-BMS", &a.email, &raw), "secret": totp::base32_encode(&raw) }))
        }
        "email" => {
            if !st.mailer.is_enabled() {
                return Err(AppError::BadRequest("email codes are not available: the email settings have not been set up".into()));
            }
            (Some(six_digits()), None, json!({ "sent_to": mask_email(&a.email) }))
        }
        _ => {
            if !st.sms.is_enabled() {
                return Err(AppError::BadRequest("text-message codes are not available: the SMS settings have not been set up".into()));
            }
            let phone = req
                .phone
                .as_deref()
                .map(str::trim)
                .filter(|p| (8..=20).contains(&p.len()))
                .ok_or_else(|| AppError::BadRequest("enter the phone number to send codes to".into()))?;
            (Some(six_digits()), Some(phone.to_string()), json!({ "sent_to": mask_phone(phone) }))
        }
    };
    sqlx::query(
        "INSERT INTO admin_challenges (id, tenant_id, admin_id, purpose, method, code_hash, target, expires_at)
         VALUES ($1, $2, $3, 'enrol', $4, $5, $6, now() + make_interval(mins => $7))",
    )
    .bind(challenge)
    .bind(a.tenant)
    .bind(a.id)
    .bind(&req.method)
    .bind(code.as_deref().map(|c| code_hash(challenge, c)))
    .bind(&target)
    .bind(CODE_MINUTES)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    if let Some(c) = code.as_deref() {
        deliver(st, a.tenant, &req.method, &a.email, target.as_deref(), c).await?;
    }
    let mut out = json!({ "challenge": challenge, "method": req.method });
    if let (Some(o), Some(e)) = (out.as_object_mut(), extra.as_object()) {
        o.extend(e.clone());
    }
    Ok(out)
}

pub async fn mfa_confirm(st: &AppState, tenant: &str, h: &HeaderMap, req: Verify) -> Result<Value> {
    let a = authenticate(st, tenant, h, Perm::SelfService).await?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let ch = sqlx::query(
        "SELECT id, method, code_hash, target, attempts FROM admin_challenges
          WHERE id = $1 AND admin_id = $2 AND purpose = 'enrol' AND used_at IS NULL AND expires_at > now() FOR UPDATE",
    )
    .bind(req.challenge)
    .bind(a.id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::BadRequest("that setup has expired. Start again".into()))?;
    if !check_challenge(st, &mut tx, &ch, None, &req.code).await? {
        tx.commit().await?;
        return Err(AppError::BadRequest("that code is not right. Check it and try again".into()));
    }
    let method: String = ch.try_get("method")?;
    let target: Option<String> = ch.try_get("target")?;
    match method.as_str() {
        "totp" => {
            sqlx::query("UPDATE admin_users SET totp_secret = $2, totp_enabled = true, mfa_method = 'totp' WHERE id = $1")
                .bind(a.id)
                .bind(target.unwrap_or_default().into_bytes())
                .execute(&mut *tx)
                .await?;
        }
        "email" => {
            sqlx::query("UPDATE admin_users SET mfa_method = 'email', totp_enabled = false WHERE id = $1").bind(a.id).execute(&mut *tx).await?;
        }
        _ => {
            sqlx::query("UPDATE admin_users SET mfa_method = 'sms', phone = $2, totp_enabled = false WHERE id = $1")
                .bind(a.id)
                .bind(target)
                .execute(&mut *tx)
                .await?;
        }
    }
    audit(&mut tx, a.tenant, "admin", Some(&a.actor()), "admin.mfa_enabled", Some(("admin", &a.actor())), json!({ "method": method })).await?;
    tx.commit().await?;
    Ok(json!({ "ok": true, "mfa_method": method }))
}

#[derive(Debug, Deserialize)]
pub struct ChangePassword {
    pub current: String,
    pub new: String,
}

pub async fn change_password(st: &AppState, tenant: &str, h: &HeaderMap, req: ChangePassword) -> Result<Value> {
    let a = authenticate(st, tenant, h, Perm::SelfService).await?;
    st.limiter.hit(&format!("pw-change:{}", a.id), 8, std::time::Duration::from_secs(600))?;
    check_password_policy(&req.new, &a.email)?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM admin_users WHERE id = $1").bind(a.id).fetch_one(&mut *tx).await?;
    if !verify_password(&hash, &req.current) {
        return Err(AppError::BadRequest("your current password is not right".into()));
    }
    sqlx::query("UPDATE admin_users SET password_hash = $2 WHERE id = $1").bind(a.id).bind(hash_password(&req.new)?).execute(&mut *tx).await?;
    audit(&mut tx, a.tenant, "admin", Some(&a.actor()), "admin.password_changed", Some(("admin", &a.actor())), json!({})).await?;
    tx.commit().await?;
    Ok(json!({ "ok": true }))
}

// ---------------------------------------------------------------------------
// Managing staff (system admin: anyone; CEO: admins and customer reps)
// ---------------------------------------------------------------------------

pub async fn list_users(st: &AppState, tenant: &str, h: &HeaderMap, include_closed: bool) -> Result<Value> {
    let a = authenticate(st, tenant, h, Perm::Users).await?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let v: Value = sqlx::query_scalar(
        "SELECT COALESCE(jsonb_agg(t), '[]'::jsonb) FROM (
           SELECT u.id, u.email, u.display_name, u.role, u.status::text AS status, u.mfa_method, u.phone IS NOT NULL AS has_phone,
                  u.created_at, u.last_login_at, (SELECT x.email FROM admin_users x WHERE x.id = u.created_by) AS created_by,
                  (SELECT count(*) FROM audit_log l WHERE l.actor_id = u.id::text) AS actions
             FROM admin_users u WHERE ($1 OR u.status <> 'closed') ORDER BY u.role, u.email) t",
    )
    .bind(include_closed)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(v)
}

#[derive(Debug, Deserialize)]
pub struct NewUser {
    pub email: String,
    pub password: String,
    pub role: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
}

fn valid_email(e: &str) -> bool {
    e.len() <= 200 && e.contains('@') && !e.contains(char::is_whitespace) && e.split_once('@').is_some_and(|(l, d)| !l.is_empty() && d.contains('.'))
}

pub async fn create_user(st: &AppState, tenant: &str, h: &HeaderMap, req: NewUser) -> Result<Value> {
    let a = authenticate(st, tenant, h, Perm::Users).await?;
    let role = Role::parse(&req.role).ok_or_else(|| AppError::BadRequest(format!("role must be one of {}", ROLE_NAMES.join(", "))))?;
    if !a.role.manages(role) {
        return Err(AppError::Forbidden);
    }
    let email = req.email.trim().to_ascii_lowercase();
    if !valid_email(&email) {
        return Err(AppError::BadRequest("enter a valid email address".into()));
    }
    check_password_policy(&req.password, &email)?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO admin_users (tenant_id, email, password_hash, role, display_name, phone, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(a.tenant)
    .bind(&email)
    .bind(hash_password(&req.password)?)
    .bind(role.as_str())
    .bind(req.display_name.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(|s| s.chars().take(80).collect::<String>()))
    .bind(req.phone.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(|s| s.chars().take(20).collect::<String>()))
    .bind(a.id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => AppError::BadRequest("a staff account with that email already exists".into()),
        _ => e.into(),
    })?;
    audit(&mut tx, a.tenant, "admin", Some(&a.actor()), "staff.created", Some(("admin", &id.to_string())), json!({ "email": email, "role": role.as_str() }))
        .await?;
    tx.commit().await?;
    Ok(json!({ "id": id }))
}

#[derive(Debug, Deserialize)]
pub struct UserPatch {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

/// Load a target, check the actor may manage them, and make sure the last system admin is never lost.
async fn target_for_management(tx: &mut PgConnection, a: &AdminCtx, id: Uuid) -> Result<(Role, String)> {
    let r = sqlx::query("SELECT role, status::text AS status FROM admin_users WHERE id = $1 FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(AppError::NotFound)?;
    let role = Role::parse(&r.try_get::<String, _>("role")?).ok_or_else(|| AppError::Internal("unknown staff role".into()))?;
    if !a.role.manages(role) {
        return Err(AppError::Forbidden);
    }
    Ok((role, r.try_get("status")?))
}

async fn another_active_system_admin(tx: &mut PgConnection, except: Uuid) -> Result<bool> {
    Ok(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM admin_users WHERE role = 'system_admin' AND status = 'active' AND id <> $1")
        .bind(except)
        .fetch_one(tx)
        .await?
        > 0)
}

pub async fn update_user(st: &AppState, tenant: &str, h: &HeaderMap, id: Uuid, p: UserPatch) -> Result<Value> {
    let a = authenticate(st, tenant, h, Perm::Users).await?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let (cur, _) = target_for_management(&mut tx, &a, id).await?;
    if let Some(nr) = p.role.as_deref() {
        let nr = Role::parse(nr).ok_or_else(|| AppError::BadRequest("unknown role".into()))?;
        if !a.role.manages(nr) {
            return Err(AppError::Forbidden);
        }
        if cur == Role::SystemAdmin && nr != Role::SystemAdmin && !another_active_system_admin(&mut tx, id).await? {
            return Err(AppError::BadRequest("there must always be at least one active system administrator".into()));
        }
        sqlx::query("UPDATE admin_users SET role = $2 WHERE id = $1").bind(id).bind(nr.as_str()).execute(&mut *tx).await?;
    }
    if let Some(s) = p.status.as_deref() {
        if !["active", "suspended"].contains(&s) {
            return Err(AppError::BadRequest("status must be active or suspended".into()));
        }
        if s == "suspended" {
            if id == a.id {
                return Err(AppError::BadRequest("you cannot suspend your own account".into()));
            }
            if cur == Role::SystemAdmin && !another_active_system_admin(&mut tx, id).await? {
                return Err(AppError::BadRequest("there must always be at least one active system administrator".into()));
            }
            sqlx::query("UPDATE admin_sessions SET revoked_at = now() WHERE admin_user_id = $1 AND revoked_at IS NULL").bind(id).execute(&mut *tx).await?;
        }
        sqlx::query("UPDATE admin_users SET status = $2::account_status WHERE id = $1 AND status <> 'closed'").bind(id).bind(s).execute(&mut *tx).await?;
    }
    if let Some(n) = p.display_name.as_deref() {
        sqlx::query("UPDATE admin_users SET display_name = $2 WHERE id = $1")
            .bind(id)
            .bind(n.trim().chars().take(80).collect::<String>())
            .execute(&mut *tx)
            .await?;
    }
    if let Some(ph) = p.phone.as_deref() {
        sqlx::query("UPDATE admin_users SET phone = NULLIF($2, '') WHERE id = $1")
            .bind(id)
            .bind(ph.trim().chars().take(20).collect::<String>())
            .execute(&mut *tx)
            .await?;
    }
    audit(&mut tx, a.tenant, "admin", Some(&a.actor()), "staff.updated", Some(("admin", &id.to_string())), json!({ "role": p.role, "status": p.status }))
        .await?;
    tx.commit().await?;
    Ok(json!({ "ok": true }))
}

/// "Delete": the account is closed and every session ended. The row stays so the audit log still says who did what.
pub async fn delete_user(st: &AppState, tenant: &str, h: &HeaderMap, id: Uuid) -> Result<Value> {
    let a = authenticate(st, tenant, h, Perm::Users).await?;
    if id == a.id {
        return Err(AppError::BadRequest("you cannot delete your own account".into()));
    }
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let (role, _) = target_for_management(&mut tx, &a, id).await?;
    if role == Role::SystemAdmin && !another_active_system_admin(&mut tx, id).await? {
        return Err(AppError::BadRequest("there must always be at least one active system administrator".into()));
    }
    sqlx::query("UPDATE admin_users SET status = 'closed' WHERE id = $1").bind(id).execute(&mut *tx).await?;
    sqlx::query("UPDATE admin_sessions SET revoked_at = now() WHERE admin_user_id = $1 AND revoked_at IS NULL").bind(id).execute(&mut *tx).await?;
    audit(&mut tx, a.tenant, "admin", Some(&a.actor()), "staff.deleted", Some(("admin", &id.to_string())), json!({})).await?;
    tx.commit().await?;
    Ok(json!({ "ok": true }))
}

#[derive(Debug, Deserialize)]
pub struct ResetPassword {
    pub password: String,
}

pub async fn reset_password(st: &AppState, tenant: &str, h: &HeaderMap, id: Uuid, req: ResetPassword) -> Result<Value> {
    let a = authenticate(st, tenant, h, Perm::Users).await?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    target_for_management(&mut tx, &a, id).await?;
    let email: String = sqlx::query_scalar("SELECT email FROM admin_users WHERE id = $1").bind(id).fetch_one(&mut *tx).await?;
    check_password_policy(&req.password, &email)?;
    sqlx::query("UPDATE admin_users SET password_hash = $2, failed_logins = 0, locked_until = NULL WHERE id = $1")
        .bind(id)
        .bind(hash_password(&req.password)?)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE admin_sessions SET revoked_at = now() WHERE admin_user_id = $1 AND revoked_at IS NULL").bind(id).execute(&mut *tx).await?;
    audit(&mut tx, a.tenant, "admin", Some(&a.actor()), "staff.password_reset", Some(("admin", &id.to_string())), json!({})).await?;
    tx.commit().await?;
    Ok(json!({ "ok": true }))
}

/// For someone who lost their phone: they must set up 2-step verification again at next sign-in.
pub async fn reset_mfa(st: &AppState, tenant: &str, h: &HeaderMap, id: Uuid) -> Result<Value> {
    let a = authenticate(st, tenant, h, Perm::Users).await?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    target_for_management(&mut tx, &a, id).await?;
    sqlx::query("UPDATE admin_users SET mfa_method = 'none', totp_enabled = false, totp_secret = NULL WHERE id = $1").bind(id).execute(&mut *tx).await?;
    sqlx::query("UPDATE admin_sessions SET revoked_at = now() WHERE admin_user_id = $1 AND revoked_at IS NULL").bind(id).execute(&mut *tx).await?;
    audit(&mut tx, a.tenant, "admin", Some(&a.actor()), "staff.mfa_reset", Some(("admin", &id.to_string())), json!({})).await?;
    tx.commit().await?;
    Ok(json!({ "ok": true }))
}

/// What one staff member has done (system admin and CEO).
pub async fn user_activity(st: &AppState, tenant: &str, h: &HeaderMap, id: Uuid, before: Option<i64>, limit: Option<i64>) -> Result<Value> {
    let a = authenticate(st, tenant, h, Perm::Activity).await?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let v: Value = sqlx::query_scalar(
        "SELECT COALESCE(jsonb_agg(t), '[]'::jsonb) FROM (
           SELECT seq, ts, action, entity, entity_id, ip::text AS ip, detail FROM audit_log
            WHERE actor_id = $1 AND ($2::bigint IS NULL OR seq < $2) ORDER BY seq DESC LIMIT $3) t",
    )
    .bind(id.to_string())
    .bind(before)
    .bind(limit.unwrap_or(100).clamp(1, 500))
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_six_digits() {
        for _ in 0..200 {
            let c = six_digits();
            assert_eq!(c.len(), 6);
            assert!(c.chars().all(|x| x.is_ascii_digit()));
        }
    }

    #[test]
    fn code_hash_is_bound_to_its_challenge() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        assert_ne!(code_hash(a, "123456"), code_hash(b, "123456"));
        assert_eq!(code_hash(a, "123456"), code_hash(a, "123456"));
    }

    #[test]
    fn destinations_are_masked() {
        assert_eq!(mask_email("garba@example.com"), "g***@example.com");
        assert_eq!(mask_phone("08031234567"), "080***567");
    }

    #[test]
    fn emails_are_checked_loosely() {
        assert!(valid_email("a@b.co") && !valid_email("a@b") && !valid_email("a b@c.com") && !valid_email("@c.com"));
    }
}
