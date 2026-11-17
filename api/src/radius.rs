//! The link between paid plans and the router.
//!
//! ```text
//! customer ──code or sign-in──▶ portal ──POST /connect──▶ ticket (user + one-time password, bound to the MAC)
//! hotspot  ──username/password──▶ FreeRADIUS ──▶ site agent ──POST /node/radius/authorize──▶ limits for the router
//! hotspot  ──Start / Interim / Stop (every 60 s)──▶ site agent ──POST /node/radius/accounting──▶ continue or disconnect
//! ```
//!
//! Why tickets instead of passing the access code through RADIUS: the portal
//! has already checked the code or the member's sign-in, so RADIUS never sees
//! a long-lived secret. A ticket works once, for one MAC address, for two
//! minutes, and RADIUS passwords travelling to the router are worthless
//! afterwards.
//!
//! Every second of "active time" and every byte is charged by the entitlement
//! engine in `accounting()`. The router only carries out what we answer.

use axum::http::HeaderMap;
use entitlement::radius::{mikrotik_rate_limit, reply_attributes};
use entitlement::{apply_usage, select_index, Action, BindError, DeviceSlots, DisconnectReason, SessionState, Status, UsageSample};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use crate::auth::{bearer, ct_eq, new_token, sha256_hex};
use crate::codes::{new_access_code, normalise_code};
use crate::db::{audit, tenant_id_by_slug, tenant_tx};
use crate::entitlements_db::{load_for_update, load_usable_for_update, now_secs, save, site_tz_offset, Subject};
use crate::error::{AppError, Result};
use crate::fingerprint::{identify, Clues};
use crate::tether::{assess, Observation, Response};
use crate::AppState;

/// Who the site agent is, proven by its bearer token.
#[derive(Debug, Clone, Copy)]
pub struct NodeCtx {
    pub tenant: Uuid,
    pub site_id: Uuid,
    pub token_id: Uuid,
}

/// Check the agent's token. The database function returns the tenant and site
/// for a valid, unrevoked token and nothing else.
pub async fn authenticate_node(state: &AppState, headers: &HeaderMap) -> Result<NodeCtx> {
    let token = bearer(headers).ok_or(AppError::Unauthorised)?;
    if !token.starts_with("nt_") {
        return Err(AppError::Unauthorised);
    }
    let row = sqlx::query("SELECT token_id, tenant_id, site_id FROM node_token_lookup($1)")
        .bind(sha256_hex(token))
        .fetch_optional(&state.pool)
        .await?
        .ok_or(AppError::Unauthorised)?;
    let ctx = NodeCtx { token_id: row.try_get("token_id")?, tenant: row.try_get("tenant_id")?, site_id: row.try_get("site_id")? };
    let mut tx = tenant_tx(&state.pool, ctx.tenant).await?;
    sqlx::query("UPDATE node_tokens SET last_used_at = now() WHERE id = $1").bind(ctx.token_id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(ctx)
}

/// `AA-BB-CC-DD-EE-FF`, `aabb.ccdd.eeff` and `AA:BB:..` all become `aa:bb:cc:dd:ee:ff`.
pub fn normalise_mac(input: &str) -> Result<String> {
    let hex: String = input.chars().filter(char::is_ascii_hexdigit).collect::<String>().to_ascii_lowercase();
    let only_separators = input.chars().all(|c| c.is_ascii_hexdigit() || matches!(c, ':' | '-' | '.'));
    if hex.len() != 12 || !only_separators {
        return Err(AppError::BadRequest("invalid device address".into()));
    }
    Ok(hex.as_bytes().chunks(2).map(|c| std::str::from_utf8(c).unwrap()).collect::<Vec<_>>().join(":"))
}

// ---------------------------------------------------------------------------
// Connect: portal asks for a ticket
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ConnectRequest {
    pub site: String,
    /// The device's address as the hotspot reported it.
    pub mac: String,
    /// Guest access code. Members send their bearer token instead.
    #[serde(default)]
    pub code: Option<String>,
    /// The customer ticked the terms and privacy box.
    #[serde(default)]
    pub accept_terms: bool,
    #[serde(default)]
    pub dhcp_hostname: Option<String>,
}

/// What the portal hands to the hotspot login form.
#[derive(Debug, Serialize)]
pub struct Ticket {
    pub username: String,
    pub password: String,
    pub expires_in_secs: u32,
}

pub const TICKET_TTL_SECS: i64 = 120;
pub const TERMS_VERSION: &str = "2026-11";

/// Guest sign-in by access code.
pub async fn connect_with_code(state: &AppState, tenant_slug: &str, headers: &HeaderMap, ip: Option<&str>, req: ConnectRequest) -> Result<Ticket> {
    let code = normalise_code(req.code.as_deref().unwrap_or(""));
    if code.len() != 8 {
        return Err(AppError::BadRequest("enter your 8-character access code".into()));
    }
    // Guessing guard: per address and per code. 8 characters from 32 is far
    // out of reach by guessing, but we do not let anyone try.
    let who = ip.unwrap_or("unknown");
    state.limiter.hit(&format!("code-ip:{who}"), 20, std::time::Duration::from_secs(600))?;
    state.limiter.hit(&format!("code:{tenant_slug}:{code}"), 10, std::time::Duration::from_secs(600))?;

    let tenant = tenant_id_by_slug(&state.pool, tenant_slug).await?;
    let mut tx = tenant_tx(&state.pool, tenant).await?;

    let code_id: Option<Uuid> = sqlx::query_scalar("SELECT id FROM access_codes WHERE code = $1").bind(&code).fetch_optional(&mut *tx).await?;
    let Some(code_id) = code_id else {
        audit(&mut tx, tenant, "guest", ip, "auth.code_rejected", None, serde_json::json!({})).await?;
        tx.commit().await?;
        return Err(AppError::BadRequest("that code was not recognised".into()));
    };
    let ticket = create_ticket(&mut tx, tenant, &req, Subject::Code(code_id), headers, ip).await?;
    tx.commit().await?;
    Ok(ticket)
}

/// Shared by guest and member sign-in: check the subject can use a plan now,
/// record consent and the device, and issue the ticket.
pub async fn create_ticket(
    conn: &mut PgConnection,
    tenant: Uuid,
    req: &ConnectRequest,
    subject: Subject,
    headers: &HeaderMap,
    ip: Option<&str>,
) -> Result<Ticket> {
    if !req.accept_terms {
        return Err(AppError::BadRequest("please accept the terms of use to connect".into()));
    }
    let mac = normalise_mac(&req.mac)?;
    let site_id: Uuid = sqlx::query_scalar("SELECT id FROM sites WHERE slug = $1 AND status = 'active'")
        .bind(&req.site)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(AppError::NotFound)?;

    // Is there anything to connect with right now?
    let now = now_secs();
    let mut ents = load_usable_for_update(conn, subject).await?;
    for l in ents.iter_mut() {
        l.ent.refresh_status(now);
    }
    if select_index(&ents.iter().map(|l| l.ent.clone()).collect::<Vec<_>>(), now).is_none() {
        return Err(AppError::BadRequest("there is no time or data left on this plan. Buy a new plan to continue".into()));
    }

    // The device and what we can tell about it.
    let ua = headers.get("user-agent").and_then(|v| v.to_str().ok()).map(|s| s.chars().take(400).collect::<String>());
    let info = identify(&Clues { mac: Some(&mac), user_agent: ua.as_deref(), dhcp_hostname: req.dhcp_hostname.as_deref(), dhcp_vendor_class: None });
    upsert_device(conn, tenant, &mac, req.dhcp_hostname.as_deref(), None, ua.as_deref(), &info).await?;

    let (account_id, code_id) = match subject {
        Subject::Account(a) => (Some(a), None),
        Subject::Code(c) => (None, Some(c)),
    };
    sqlx::query(
        "INSERT INTO consents (tenant_id, account_id, access_code_id, terms_version, ip, user_agent)
         VALUES ($1, $2, $3, $4, $5::inet, $6)",
    )
    .bind(tenant)
    .bind(account_id)
    .bind(code_id)
    .bind(TERMS_VERSION)
    .bind(ip)
    .bind(ua.as_deref())
    .execute(&mut *conn)
    .await?;

    // Ticket: random username and password, only the password's hash is stored.
    let username = format!("tk{}", new_access_code().to_ascii_lowercase());
    let password = new_token("tp");
    sqlx::query(
        "INSERT INTO connect_tickets (tenant_id, site_id, username, password_hash, mac, account_id, access_code_id, expires_at)
         VALUES ($1, $2, $3, $4, $5::macaddr, $6, $7, now() + make_interval(secs => $8))",
    )
    .bind(tenant)
    .bind(site_id)
    .bind(&username)
    .bind(sha256_hex(&password))
    .bind(&mac)
    .bind(account_id)
    .bind(code_id)
    .bind(TICKET_TTL_SECS as f64)
    .execute(&mut *conn)
    .await?;
    audit(
        conn,
        tenant,
        if account_id.is_some() { "account" } else { "guest" },
        ip,
        "auth.ticket_issued",
        Some(("device", &mac)),
        serde_json::json!({ "site": req.site, "os": info.os_family, "class": info.device_class, "mac_randomised": info.mac_randomised }),
    )
    .await?;
    Ok(Ticket { username, password, expires_in_secs: TICKET_TTL_SECS as u32 })
}

/// Insert or refresh a device. A better (more confident) identification wins;
/// a weaker one never overwrites it.
async fn upsert_device(
    conn: &mut PgConnection,
    tenant: Uuid,
    mac: &str,
    hostname: Option<&str>,
    vendor_class: Option<&str>,
    ua: Option<&str>,
    info: &crate::fingerprint::DeviceInfo,
) -> Result<Uuid> {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO devices (tenant_id, mac, mac_randomised, hostname, dhcp_vendor_class, os_family, os_version, device_class, user_agent, confidence)
         VALUES ($1, $2::macaddr, $3, $4, $5, $6, $7, $8, $9, $10)
         ON CONFLICT (tenant_id, mac) DO UPDATE SET
            last_seen_at      = now(),
            hostname          = COALESCE(EXCLUDED.hostname, devices.hostname),
            dhcp_vendor_class = COALESCE(EXCLUDED.dhcp_vendor_class, devices.dhcp_vendor_class),
            user_agent        = COALESCE(EXCLUDED.user_agent, devices.user_agent),
            os_family   = CASE WHEN EXCLUDED.confidence >= devices.confidence THEN COALESCE(EXCLUDED.os_family, devices.os_family) ELSE devices.os_family END,
            os_version  = CASE WHEN EXCLUDED.confidence >= devices.confidence THEN COALESCE(EXCLUDED.os_version, devices.os_version) ELSE devices.os_version END,
            device_class= CASE WHEN EXCLUDED.confidence >= devices.confidence THEN EXCLUDED.device_class ELSE devices.device_class END,
            confidence  = GREATEST(EXCLUDED.confidence, devices.confidence)
         RETURNING id",
    )
    .bind(tenant)
    .bind(mac)
    .bind(info.mac_randomised)
    .bind(hostname)
    .bind(vendor_class)
    .bind(&info.os_family)
    .bind(&info.os_version)
    .bind(&info.device_class)
    .bind(ua)
    .bind(info.confidence as i16)
    .fetch_one(conn)
    .await?;
    Ok(id)
}

// ---------------------------------------------------------------------------
// Authorize: the router asks "may this ticket connect, and with what limits?"
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct AuthorizeRequest {
    pub username: String,
    pub password: String,
    pub mac: String,
    #[serde(default)]
    pub dhcp_hostname: Option<String>,
    #[serde(default)]
    pub dhcp_vendor_class: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum AuthorizeReply {
    Accept { entitlement_id: Uuid, attributes: Vec<(String, String)> },
    Reject { reason: String },
}

fn reject(reason: &str) -> AuthorizeReply {
    AuthorizeReply::Reject { reason: reason.to_string() }
}

pub async fn authorize(state: &AppState, node: NodeCtx, req: AuthorizeRequest) -> Result<AuthorizeReply> {
    let mac = normalise_mac(&req.mac)?;
    let mut tx = tenant_tx(&state.pool, node.tenant).await?;
    let now = now_secs();

    let t = sqlx::query(
        "SELECT id, site_id, password_hash, mac::text AS mac, account_id, access_code_id,
                (expires_at < now()) AS expired, (authorised_at IS NOT NULL) AS used
           FROM connect_tickets WHERE username = $1 FOR UPDATE",
    )
    .bind(&req.username)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(t) = t else {
        tx.rollback().await?;
        return Ok(reject("unknown ticket"));
    };
    let ticket_id: Uuid = t.try_get("id")?;
    let ok = t.try_get::<Uuid, _>("site_id")? == node.site_id
        && ct_eq(&t.try_get::<String, _>("password_hash")?, &sha256_hex(&req.password))
        && t.try_get::<String, _>("mac")? == mac
        && !t.try_get::<bool, _>("expired")?
        && !t.try_get::<bool, _>("used")?;
    if !ok {
        // One reason for every failure, so a guesser learns nothing.
        audit(
            &mut tx,
            node.tenant,
            "node",
            None,
            "radius.reject",
            Some(("ticket", &ticket_id.to_string())),
            serde_json::json!({ "why": "ticket check failed" }),
        )
        .await?;
        tx.commit().await?;
        return Ok(reject("ticket not valid"));
    }

    let account_id: Option<Uuid> = t.try_get("account_id")?;
    let code_id: Option<Uuid> = t.try_get("access_code_id")?;
    let subject = match (account_id, code_id) {
        (Some(a), _) => Subject::Account(a),
        (None, Some(c)) => Subject::Code(c),
        _ => return Err(AppError::Internal("ticket has no subject".into())),
    };

    // Pick the purchase that expires soonest, and refresh stale statuses.
    let mut loaded = load_usable_for_update(&mut tx, subject).await?;
    for l in loaded.iter_mut() {
        l.ent.refresh_status(now);
        save(&mut tx, l).await?;
    }
    let plain: Vec<_> = loaded.iter().map(|l| l.ent.clone()).collect();
    let Some(idx) = select_index(&plain, now) else {
        tx.commit().await?;
        return Ok(reject("no time or data left"));
    };
    let mut chosen = loaded.swap_remove(idx);
    if chosen.ent.status == Status::Pending {
        chosen.ent.activate(now); // the validity window starts at first connection
    }

    // The device: refine what we know with what the router saw in DHCP.
    let dev_row = sqlx::query("SELECT id, user_agent FROM devices WHERE mac = $1::macaddr").bind(&mac).fetch_optional(&mut *tx).await?;
    let ua: Option<String> = match &dev_row {
        Some(r) => r.try_get("user_agent")?,
        None => None,
    };
    let info = identify(&Clues {
        mac: Some(&mac),
        user_agent: ua.as_deref(),
        dhcp_hostname: req.dhcp_hostname.as_deref(),
        dhcp_vendor_class: req.dhcp_vendor_class.as_deref(),
    });
    let device_id = upsert_device(&mut tx, node.tenant, &mac, req.dhcp_hostname.as_deref(), req.dhcp_vendor_class.as_deref(), ua.as_deref(), &info).await?;

    // Device slots.
    let mut slots = DeviceSlots::new(chosen.ent.plan.device_limit, chosen.ent.plan.swap_cooldown_secs);
    slots.last_swap_at = chosen.last_swap_at;
    let bound = sqlx::query(
        "SELECT device_id::text AS d, extract(epoch FROM bound_at)::bigint AS at FROM device_bindings WHERE entitlement_id = $1 AND released_at IS NULL",
    )
    .bind(chosen.id)
    .fetch_all(&mut *tx)
    .await?;
    for b in &bound {
        slots.bindings.push(entitlement::slots::Binding { device_id: b.try_get("d")?, bound_at: b.try_get("at")? });
    }
    match slots.bind(&device_id.to_string(), now) {
        Ok(entitlement::BindOutcome::Bound) => {
            sqlx::query("INSERT INTO device_bindings (tenant_id, entitlement_id, device_id) VALUES ($1, $2, $3)")
                .bind(node.tenant)
                .bind(chosen.id)
                .bind(device_id)
                .execute(&mut *tx)
                .await?;
        }
        Ok(entitlement::BindOutcome::AlreadyBound) => {}
        Err(BindError::SlotsFull { can_swap_now, .. }) => {
            sqlx::query("INSERT INTO tethering_flags (tenant_id, device_id, signal, score, action, detail) VALUES ($1, $2, 'slot_overflow', 10, 'none', $3)")
                .bind(node.tenant)
                .bind(device_id)
                .bind(serde_json::json!({ "entitlement_id": chosen.id, "can_swap_now": can_swap_now }))
                .execute(&mut *tx)
                .await?;
            audit(
                &mut tx,
                node.tenant,
                "node",
                None,
                "radius.reject",
                Some(("device", &mac)),
                serde_json::json!({ "why": "device limit", "entitlement_id": chosen.id }),
            )
            .await?;
            tx.commit().await?;
            return Ok(reject("device limit reached for this plan"));
        }
    }

    let tz = site_tz_offset(&mut tx, node.site_id).await?;
    let limits = match chosen.ent.session_limits(now, tz) {
        Ok(l) => l,
        Err(_) => {
            tx.commit().await?;
            return Ok(reject("no time or data left"));
        }
    };

    save(&mut tx, &chosen).await?;
    sqlx::query("UPDATE connect_tickets SET authorised_at = now(), entitlement_id = $2, device_id = $3 WHERE id = $1")
        .bind(ticket_id)
        .bind(chosen.id)
        .bind(device_id)
        .execute(&mut *tx)
        .await?;
    audit(
        &mut tx,
        node.tenant,
        "node",
        None,
        "radius.accept",
        Some(("entitlement", &chosen.id.to_string())),
        serde_json::json!({ "device": mac, "os": info.os_family, "throttled": limits.throttled }),
    )
    .await?;
    tx.commit().await?;
    Ok(AuthorizeReply::Accept { entitlement_id: chosen.id, attributes: reply_attributes(&limits) })
}

// ---------------------------------------------------------------------------
// Accounting: Start, Interim (every 60 s) and Stop
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct TetherObservation {
    #[serde(default)]
    pub ttl_lowered_ratio: f32,
    #[serde(default)]
    pub distinct_os_families: u32,
    #[serde(default)]
    pub connection_count: u32,
}

#[derive(Debug, Deserialize)]
pub struct AccountingRequest {
    /// `start`, `interim` or `stop`.
    pub status: String,
    pub acct_session_id: String,
    pub username: String,
    pub mac: String,
    #[serde(default)]
    pub ip: Option<String>,
    /// Running totals for the session, as the router counts them (gigawords already added in).
    #[serde(default)]
    pub bytes_in: u64,
    #[serde(default)]
    pub bytes_out: u64,
    #[serde(default)]
    pub session_secs: u32,
    #[serde(default)]
    pub ssid: Option<String>,
    #[serde(default)]
    pub terminate_cause: Option<String>,
    #[serde(default)]
    pub observation: Option<TetherObservation>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct AccountingReply {
    /// `continue` or `disconnect`.
    pub action: &'static str,
    pub reason: Option<String>,
    /// The speed the router should be using right now (changes when fair use kicks in).
    pub rate_limit: Option<String>,
    /// Remaining seconds the router should allow, refreshed each update.
    pub session_timeout_secs: Option<u32>,
}

fn cont(rate: Option<String>, timeout: Option<u32>) -> AccountingReply {
    AccountingReply { action: "continue", reason: None, rate_limit: rate, session_timeout_secs: timeout }
}

fn stop(reason: &str) -> AccountingReply {
    AccountingReply { action: "disconnect", reason: Some(reason.to_string()), rate_limit: None, session_timeout_secs: None }
}

fn end_reason_for(r: DisconnectReason) -> &'static str {
    match r {
        DisconnectReason::Idle => "idle",
        DisconnectReason::TimeExhausted => "time_exhausted",
        DisconnectReason::DataExhausted => "data_exhausted",
        DisconnectReason::Expired => "expired",
    }
}

fn end_reason_from_cause(cause: Option<&str>) -> &'static str {
    match cause.unwrap_or("").to_ascii_lowercase().as_str() {
        "user-request" => "user",
        "idle-timeout" => "idle",
        "session-timeout" => "time_exhausted",
        "admin-reset" | "admin-reboot" => "admin",
        "lost-carrier" | "lost-service" | "nas-error" | "nas-reboot" | "port-error" => "node_lost",
        _ => "error",
    }
}

/// Throttled speed when we slow a sharing device: the plan's own fair-use
/// profile if it has one, otherwise a fixed 1 Mbps.
const FALLBACK_THROTTLE: &str = "1000k/1000k";

pub async fn accounting(state: &AppState, node: NodeCtx, req: AccountingRequest) -> Result<AccountingReply> {
    let mac = normalise_mac(&req.mac)?;
    let mut tx = tenant_tx(&state.pool, node.tenant).await?;
    let reply = match req.status.as_str() {
        "start" => start(&mut tx, node, &req, &mac).await?,
        "interim" | "stop" => update(&mut tx, node, &req, &mac).await?,
        _ => return Err(AppError::BadRequest("status must be start, interim or stop".into())),
    };
    tx.commit().await?;
    Ok(reply)
}

async fn start(tx: &mut PgConnection, node: NodeCtx, req: &AccountingRequest, mac: &str) -> Result<AccountingReply> {
    let t = sqlx::query("SELECT entitlement_id, device_id FROM connect_tickets WHERE username = $1 AND site_id = $2 AND authorised_at IS NOT NULL")
        .bind(&req.username)
        .bind(node.site_id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(t) = t else { return Ok(stop("no authorised ticket for this session")) };
    let entitlement_id: Uuid = t.try_get("entitlement_id")?;
    let device_id: Uuid = t.try_get("device_id")?;
    let ip = req.ip.as_deref().filter(|s| s.parse::<std::net::IpAddr>().is_ok());
    let inserted = sqlx::query(
        "INSERT INTO sessions (tenant_id, site_id, entitlement_id, device_id, ssid, ip, acct_session_id, username)
         VALUES ($1, $2, $3, $4, $5, $6::inet, $7, $8)
         ON CONFLICT (site_id, acct_session_id) WHERE acct_session_id IS NOT NULL DO NOTHING
         RETURNING id",
    )
    .bind(node.tenant)
    .bind(node.site_id)
    .bind(entitlement_id)
    .bind(device_id)
    .bind(&req.ssid)
    .bind(ip)
    .bind(&req.acct_session_id)
    .bind(&req.username)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(row) = inserted {
        let sid: Uuid = row.try_get("id")?;
        audit(
            tx,
            node.tenant,
            "node",
            None,
            "session.start",
            Some(("session", &sid.to_string())),
            serde_json::json!({ "device": mac, "entitlement_id": entitlement_id, "ssid": req.ssid }),
        )
        .await?;
    }
    Ok(cont(None, None))
}

async fn update(tx: &mut PgConnection, node: NodeCtx, req: &AccountingRequest, mac: &str) -> Result<AccountingReply> {
    let now = now_secs();
    let s = sqlx::query(
        "SELECT id, entitlement_id, device_id, radius_bytes_in, radius_bytes_out, radius_seconds,
                active_seconds, idle_secs, bytes_in, bytes_out, (ended_at IS NOT NULL) AS ended
           FROM sessions WHERE site_id = $1 AND acct_session_id = $2 FOR UPDATE",
    )
    .bind(node.site_id)
    .bind(&req.acct_session_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(s) = s else { return Ok(stop("unknown session")) };
    if s.try_get::<bool, _>("ended")? {
        return Ok(stop("session already ended"));
    }
    let session_id: Uuid = s.try_get("id")?;
    let entitlement_id: Uuid = s.try_get("entitlement_id")?;
    let device_id: Uuid = s.try_get("device_id")?;
    let prev_in = s.try_get::<i64, _>("radius_bytes_in")? as u64;
    let prev_out = s.try_get::<i64, _>("radius_bytes_out")? as u64;
    let prev_secs = s.try_get::<i32, _>("radius_seconds")? as u32;

    // Router counters can restart (reboot), so a smaller total means "started over".
    let d_in = if req.bytes_in >= prev_in { req.bytes_in - prev_in } else { req.bytes_in };
    let d_out = if req.bytes_out >= prev_out { req.bytes_out - prev_out } else { req.bytes_out };
    let d_secs = if req.session_secs >= prev_secs { req.session_secs - prev_secs } else { req.session_secs };

    let tz = site_tz_offset(tx, node.site_id).await?;
    let mut loaded = load_for_update(tx, entitlement_id).await?;
    let mut state = SessionState {
        idle_secs: s.try_get::<i32, _>("idle_secs")? as u32,
        active_secs: s.try_get::<i32, _>("active_seconds")? as u32,
        bytes: (s.try_get::<i64, _>("bytes_in")? + s.try_get::<i64, _>("bytes_out")?) as u64,
    };

    let mut outcome = None;
    if d_secs > 0 || d_in + d_out > 0 {
        let o = apply_usage(&mut loaded.ent, &mut state, UsageSample { bytes_in: d_in, bytes_out: d_out, interval_secs: d_secs }, now, tz);
        if o.charged_active_secs > 0 {
            ledger(tx, node.tenant, entitlement_id, session_id, "active_seconds", i64::from(o.charged_active_secs)).await?;
        }
        if o.charged_bytes > 0 {
            ledger(tx, node.tenant, entitlement_id, session_id, "bytes", o.charged_bytes as i64).await?;
        }
        outcome = Some(o);
    }
    save(tx, &loaded).await?;

    sqlx::query(
        "UPDATE sessions SET radius_bytes_in = $2, radius_bytes_out = $3, radius_seconds = $4,
                bytes_in = bytes_in + $5, bytes_out = bytes_out + $6,
                active_seconds = $7, idle_secs = $8, last_update_at = now()
          WHERE id = $1",
    )
    .bind(session_id)
    .bind(req.bytes_in as i64)
    .bind(req.bytes_out as i64)
    .bind(req.session_secs as i32)
    .bind(d_in as i64)
    .bind(d_out as i64)
    .bind(state.active_secs as i32)
    .bind(state.idle_secs as i32)
    .execute(&mut *tx)
    .await?;

    // Sharing detection: the agent reports what it measured for this device.
    let mut tether_response = Response::None;
    if let Some(ob) = &req.observation {
        let a = assess(&Observation {
            ttl_lowered_ratio: ob.ttl_lowered_ratio,
            distinct_os_families: ob.distinct_os_families,
            connection_count: ob.connection_count,
            slot_overflow_attempts: 0,
        });
        tether_response = a.response;
        if a.score >= 30 {
            // At most one flag per device per 10 minutes, so a busy device does not flood the table.
            sqlx::query(
                "INSERT INTO tethering_flags (tenant_id, session_id, device_id, signal, score, action, detail)
                 SELECT $1, $2, $3, $4, $5, $6, $7
                  WHERE NOT EXISTS (SELECT 1 FROM tethering_flags WHERE device_id = $3 AND ts > now() - interval '10 minutes')",
            )
            .bind(node.tenant)
            .bind(session_id)
            .bind(device_id)
            .bind(a.signals.first().copied().unwrap_or("ttl"))
            .bind(a.score as i16)
            .bind(a.response.as_str())
            .bind(serde_json::json!({ "signals": a.signals, "ttl_lowered_ratio": ob.ttl_lowered_ratio, "os_families": ob.distinct_os_families, "connections": ob.connection_count }))
            .execute(&mut *tx)
            .await?;
        }
    }

    // Decide.
    let mut end: Option<(&'static str, String)> = None;
    if let Some(Outcome::Disconnect(r)) = outcome.as_ref().map(|o| match o.action {
        Action::Disconnect(r) => Outcome::Disconnect(r),
        Action::Continue => Outcome::Continue,
    }) {
        end = Some((end_reason_for(r), end_reason_for(r).replace('_', " ")));
    }
    if end.is_none() && tether_response == Response::Disconnect {
        end = Some(("tethering", "hotspot sharing detected".to_string()));
    }
    if req.status == "stop" && end.is_none() {
        end = Some((end_reason_from_cause(req.terminate_cause.as_deref()), "session stopped".to_string()));
    }

    if let Some((reason, text)) = end {
        sqlx::query("UPDATE sessions SET ended_at = now(), end_reason = $2::session_end_reason WHERE id = $1")
            .bind(session_id)
            .bind(reason)
            .execute(&mut *tx)
            .await?;
        audit(
            tx,
            node.tenant,
            "node",
            None,
            "session.end",
            Some(("session", &session_id.to_string())),
            serde_json::json!({ "reason": reason, "device": mac, "active_seconds": state.active_secs, "bytes": state.bytes }),
        )
        .await?;
        return Ok(if req.status == "stop" { cont(None, None) } else { stop(&text) });
    }

    // Still connected: tell the router what to enforce from now on.
    match loaded.ent.session_limits(now, tz) {
        Ok(l) => {
            let rate = if tether_response == Response::Throttle {
                loaded.ent.plan.fair_use.as_ref().map(|f| mikrotik_rate_limit(&f.throttled)).unwrap_or_else(|| FALLBACK_THROTTLE.to_string())
            } else {
                mikrotik_rate_limit(&l.speed)
            };
            Ok(cont(Some(rate), Some(l.session_timeout_secs)))
        }
        Err(_) => Ok(stop("no time or data left")),
    }
}

enum Outcome {
    Continue,
    Disconnect(DisconnectReason),
}

async fn ledger(tx: &mut PgConnection, tenant: Uuid, entitlement_id: Uuid, session_id: Uuid, kind: &str, amount: i64) -> Result<()> {
    sqlx::query("INSERT INTO usage_ledger (tenant_id, entitlement_id, session_id, kind, amount) VALUES ($1, $2, $3, $4, $5)")
        .bind(tenant)
        .bind(entitlement_id)
        .bind(session_id)
        .bind(kind)
        .bind(amount)
        .execute(tx)
        .await?;
    Ok(())
}
