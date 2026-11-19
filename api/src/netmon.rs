//! Live view of the network hardware: routers, access points, CPE radios and switches, with their
//! health, signal and the devices connected to each.
//!
//! The site agent polls the hardware (see `site-agent/src/devices.rs`) and sends a snapshot every minute
//! to `POST /v1/node/devices`. This module stores the latest state and a short history, decides whether each
//! device is healthy, and tells staff through the error log when that changes:
//!
//! | event code                  | when                                                              |
//! |-----------------------------|-------------------------------------------------------------------|
//! | `hardware.device_offline`   | the agent cannot reach the device, or stops hearing about it      |
//! | `hardware.device_degraded`  | weak signal, poor link, high load, loss or latency                |
//! | `hardware.device_recovered` | back to healthy (and the earlier offline/degraded entries close)  |
//!
//! One event per change, not one per minute. Staff read it on the Devices page (every role may).

use axum::http::HeaderMap;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::admin::{authenticate, Perm};
use crate::db::tenant_tx;
use crate::errlog::{record, NewEvent};
use crate::error::{AppError, Result};
use crate::radius::{authenticate_node, normalise_mac};
use crate::AppState;

pub const KINDS: &[&str] = &["router", "ap", "cpe", "switch", "controller", "other"];
/// A device that has not been heard about for this long is shown as offline.
pub const STALE_MINUTES: i32 = 3;
pub const SAMPLE_DAYS: i32 = 7;
const MAX_DEVICES: usize = 100;
const MAX_CLIENTS_PER_DEVICE: usize = 500;

// Thresholds: one place, so what "degraded" means is easy to change and to explain.
pub const CPE_WEAK_SIGNAL_DBM: f32 = -75.0;
pub const POOR_LINK_QUALITY_PCT: f32 = 60.0;
pub const HIGH_CPU_PCT: f32 = 90.0;
pub const HIGH_MEM_PCT: f32 = 92.0;
pub const HIGH_UTIL_PCT: f32 = 80.0;
pub const HIGH_LOSS_PCT: f32 = 5.0;
pub const HIGH_LATENCY_MS: f32 = 200.0;

#[derive(Debug, Default, Clone, Deserialize)]
pub struct ClientIn {
    pub mac: String,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub ip: Option<String>,
    #[serde(default)]
    pub ssid: Option<String>,
    #[serde(default)]
    pub signal_dbm: Option<f32>,
    #[serde(default)]
    pub connected_s: Option<i64>,
    #[serde(default)]
    pub rx_bytes: Option<i64>,
    #[serde(default)]
    pub tx_bytes: Option<i64>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct DeviceIn {
    /// Stable id the agent uses for this device (its MAC address, or the configured name).
    pub key: String,
    pub name: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub mac: Option<String>,
    #[serde(default)]
    pub ip: Option<String>,
    #[serde(default)]
    pub firmware: Option<String>,
    #[serde(default)]
    pub peer: Option<String>,
    /// `false` when the agent could not reach the device this round.
    #[serde(default)]
    pub reachable: Option<bool>,
    #[serde(default)]
    pub uptime_s: Option<i64>,
    #[serde(default)]
    pub cpu_pct: Option<f32>,
    #[serde(default)]
    pub mem_pct: Option<f32>,
    #[serde(default)]
    pub signal_dbm: Option<f32>,
    #[serde(default)]
    pub noise_dbm: Option<f32>,
    #[serde(default)]
    pub link_quality_pct: Option<f32>,
    #[serde(default)]
    pub tx_mbps: Option<f32>,
    #[serde(default)]
    pub rx_mbps: Option<f32>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub util_pct: Option<f32>,
    #[serde(default)]
    pub latency_ms: Option<f32>,
    #[serde(default)]
    pub loss_pct: Option<f32>,
    /// The devices connected to it. Leave out when the collector cannot tell (the previous list is kept).
    #[serde(default)]
    pub clients: Option<Vec<ClientIn>>,
    #[serde(default)]
    pub clients_count: Option<i32>,
    #[serde(default)]
    pub details: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct DevicesIn {
    pub devices: Vec<DeviceIn>,
}

/// Is this device healthy? Returns `online`, `degraded` or `offline` and the reasons, in words a
/// non-specialist can act on. Pure, so every rule is unit tested.
pub fn assess(d: &DeviceIn) -> (&'static str, Vec<String>) {
    if d.reachable == Some(false) {
        return ("offline", vec!["The site computer cannot reach it (no answer to ping or management requests). Check its power and cable.".into()]);
    }
    let kind = d.kind.as_deref().unwrap_or("other");
    let mut why = Vec::new();
    if let Some(s) = d.signal_dbm {
        if kind == "cpe" && s <= CPE_WEAK_SIGNAL_DBM {
            why.push(format!("Weak radio link: {s:.0} dBm (healthy is stronger than {CPE_WEAK_SIGNAL_DBM:.0}). Check alignment, obstruction and weather."));
        }
    }
    if let Some(q) = d.link_quality_pct {
        if q < POOR_LINK_QUALITY_PCT {
            why.push(format!("Poor link quality: {q:.0}% (below {POOR_LINK_QUALITY_PCT:.0}%)."));
        }
    }
    if let Some(c) = d.cpu_pct {
        if c >= HIGH_CPU_PCT {
            why.push(format!("CPU is very busy: {c:.0}%."));
        }
    }
    if let Some(m) = d.mem_pct {
        if m >= HIGH_MEM_PCT {
            why.push(format!("Memory is nearly full: {m:.0}%."));
        }
    }
    if let Some(u) = d.util_pct {
        if u >= HIGH_UTIL_PCT {
            why.push(format!("Radio is congested: {u:.0}% busy."));
        }
    }
    if let Some(l) = d.loss_pct {
        if l >= HIGH_LOSS_PCT {
            why.push(format!("Packet loss: {l:.0}% of test packets were lost."));
        }
    }
    if let Some(l) = d.latency_ms {
        if l >= HIGH_LATENCY_MS {
            why.push(format!("Slow to respond: {l:.0} ms."));
        }
    }
    (if why.is_empty() { "online" } else { "degraded" }, why)
}

fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}
fn opt(s: &Option<String>, n: usize) -> Option<String> {
    s.as_deref().map(str::trim).filter(|v| !v.is_empty()).map(|v| clip(v, n))
}
fn num(v: Option<f32>, lo: f32, hi: f32) -> Option<f32> {
    v.filter(|x| x.is_finite()).map(|x| x.clamp(lo, hi))
}

/// What the three states mean to the error log.
fn severity_for(kind: &str, state: &str) -> &'static str {
    match (state, kind) {
        ("offline", "router" | "ap" | "cpe" | "controller") => "critical",
        ("offline", _) => "error",
        ("degraded", _) => "warning",
        _ => "info",
    }
}

struct Change {
    state: &'static str,
    name: String,
    key: String,
    kind: String,
    ip: Option<String>,
    why: Vec<String>,
}

/// Site agent → platform: one snapshot of the hardware it can see.
pub async fn ingest(st: &AppState, headers: &HeaderMap, req: DevicesIn) -> Result<Value> {
    let n = authenticate_node(st, headers).await?;
    st.limiter.hit(&format!("node-devices:{}", n.token_id), 30, std::time::Duration::from_secs(60))?;
    if req.devices.len() > MAX_DEVICES {
        return Err(AppError::BadRequest(format!("at most {MAX_DEVICES} devices per report")));
    }
    let mut tx = tenant_tx(&st.pool, n.tenant).await?;
    let site_name: String = sqlx::query_scalar("SELECT name FROM sites WHERE id = $1").bind(n.site_id).fetch_one(&mut *tx).await?;
    let mut changes: Vec<Change> = Vec::new();
    let mut stored = 0;
    for d in req.devices {
        let key = clip(d.key.trim(), 80);
        let name = clip(d.name.trim(), 80);
        if key.is_empty() || name.is_empty() {
            continue;
        }
        let kind = d.kind.as_deref().filter(|k| KINDS.contains(k)).unwrap_or("other").to_string();
        let reachable = d.reachable != Some(false);
        let (status, why) = assess(&d);
        let new_alert = match status {
            "online" => "ok",
            s => s,
        };
        let prev: Option<(Uuid, String)> =
            sqlx::query_as("SELECT id, alert_state FROM net_devices WHERE tenant_id = $1 AND site_id = $2 AND dev_key = $3 FOR UPDATE")
                .bind(n.tenant)
                .bind(n.site_id)
                .bind(&key)
                .fetch_optional(&mut *tx)
                .await?;
        let mac = d.mac.as_deref().and_then(|m| normalise_mac(m).ok());
        let details = d.details.clone().filter(|v| v.to_string().len() < 4000).unwrap_or_else(|| json!({}));
        let clients: Option<Vec<(String, &ClientIn)>> =
            d.clients.as_ref().map(|l| l.iter().filter_map(|c| normalise_mac(&c.mac).ok().map(|m| (m, c))).take(MAX_CLIENTS_PER_DEVICE).collect());
        let count = clients.as_ref().map(|c| c.len() as i32);
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO net_devices (tenant_id, site_id, dev_key, name, kind, model, mac, ip, firmware, peer, status, reasons, reachable, uptime_s, cpu_pct, mem_pct,
                                      signal_dbm, noise_dbm, link_quality_pct, tx_mbps, rx_mbps, channel, util_pct, latency_ms, loss_pct, clients_count, details, alert_state)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24,$25,COALESCE($26, 0),$27,$28)
             ON CONFLICT (tenant_id, site_id, dev_key) DO UPDATE SET
               name = EXCLUDED.name, kind = EXCLUDED.kind,
               model = COALESCE(EXCLUDED.model, net_devices.model), mac = COALESCE(EXCLUDED.mac, net_devices.mac),
               ip = COALESCE(EXCLUDED.ip, net_devices.ip), firmware = COALESCE(EXCLUDED.firmware, net_devices.firmware), peer = COALESCE(EXCLUDED.peer, net_devices.peer),
               status = EXCLUDED.status, reasons = EXCLUDED.reasons, reachable = EXCLUDED.reachable, reported_at = now(),
               last_seen_at = CASE WHEN EXCLUDED.reachable THEN now() ELSE net_devices.last_seen_at END,
               uptime_s = CASE WHEN EXCLUDED.reachable THEN EXCLUDED.uptime_s ELSE net_devices.uptime_s END,
               cpu_pct = CASE WHEN EXCLUDED.reachable THEN EXCLUDED.cpu_pct ELSE net_devices.cpu_pct END,
               mem_pct = CASE WHEN EXCLUDED.reachable THEN EXCLUDED.mem_pct ELSE net_devices.mem_pct END,
               signal_dbm = CASE WHEN EXCLUDED.reachable THEN EXCLUDED.signal_dbm ELSE net_devices.signal_dbm END,
               noise_dbm = CASE WHEN EXCLUDED.reachable THEN EXCLUDED.noise_dbm ELSE net_devices.noise_dbm END,
               link_quality_pct = CASE WHEN EXCLUDED.reachable THEN EXCLUDED.link_quality_pct ELSE net_devices.link_quality_pct END,
               tx_mbps = CASE WHEN EXCLUDED.reachable THEN EXCLUDED.tx_mbps ELSE net_devices.tx_mbps END,
               rx_mbps = CASE WHEN EXCLUDED.reachable THEN EXCLUDED.rx_mbps ELSE net_devices.rx_mbps END,
               channel = CASE WHEN EXCLUDED.reachable THEN EXCLUDED.channel ELSE net_devices.channel END,
               util_pct = CASE WHEN EXCLUDED.reachable THEN EXCLUDED.util_pct ELSE net_devices.util_pct END,
               latency_ms = EXCLUDED.latency_ms, loss_pct = EXCLUDED.loss_pct,
               clients_count = CASE WHEN EXCLUDED.reachable THEN COALESCE($26, net_devices.clients_count) ELSE 0 END,
               details = net_devices.details || EXCLUDED.details,
               alert_state = EXCLUDED.alert_state
             RETURNING id",
        )
        .bind(n.tenant)
        .bind(n.site_id)
        .bind(&key)
        .bind(&name)
        .bind(&kind)
        .bind(opt(&d.model, 80))
        .bind(mac)
        .bind(opt(&d.ip, 60))
        .bind(opt(&d.firmware, 80))
        .bind(opt(&d.peer, 80))
        .bind(status)
        .bind(json!(why))
        .bind(reachable)
        .bind(d.uptime_s.filter(|v| *v >= 0))
        .bind(num(d.cpu_pct, 0.0, 100.0))
        .bind(num(d.mem_pct, 0.0, 100.0))
        .bind(num(d.signal_dbm, -150.0, 20.0))
        .bind(num(d.noise_dbm, -150.0, 20.0))
        .bind(num(d.link_quality_pct, 0.0, 100.0))
        .bind(num(d.tx_mbps, 0.0, 100_000.0))
        .bind(num(d.rx_mbps, 0.0, 100_000.0))
        .bind(opt(&d.channel, 40))
        .bind(num(d.util_pct, 0.0, 100.0))
        .bind(num(d.latency_ms, 0.0, 1_000_000.0))
        .bind(num(d.loss_pct, 0.0, 100.0))
        .bind(count.or(d.clients_count.filter(|c| *c >= 0)))
        .bind(details)
        .bind(new_alert)
        .fetch_one(&mut *tx)
        .await?;
        stored += 1;

        if reachable {
            sqlx::query(
                "INSERT INTO net_device_samples (tenant_id, device_id, signal_dbm, cpu_pct, mem_pct, util_pct, latency_ms, loss_pct, tx_mbps, rx_mbps, clients)
                 SELECT tenant_id, id, signal_dbm, cpu_pct, mem_pct, util_pct, latency_ms, loss_pct, tx_mbps, rx_mbps, clients_count FROM net_devices WHERE id = $1",
            )
            .bind(id)
            .execute(&mut *tx)
            .await?;
            if let Some(list) = clients.as_ref() {
                sqlx::query("DELETE FROM net_clients WHERE device_id = $1").bind(id).execute(&mut *tx).await?;
                for (cmac, c) in list {
                    sqlx::query(
                        "INSERT INTO net_clients (tenant_id, device_id, mac, hostname, ip, ssid, signal_dbm, connected_s, rx_bytes, tx_bytes) VALUES ($1,$2,$3::macaddr,$4,$5,$6,$7,$8,$9,$10)",
                    )
                    .bind(n.tenant)
                    .bind(id)
                    .bind(cmac)
                    .bind(opt(&c.hostname, 80))
                    .bind(opt(&c.ip, 60))
                    .bind(opt(&c.ssid, 60))
                    .bind(num(c.signal_dbm, -150.0, 20.0))
                    .bind(c.connected_s.filter(|v| *v >= 0))
                    .bind(c.rx_bytes.filter(|v| *v >= 0))
                    .bind(c.tx_bytes.filter(|v| *v >= 0))
                    .execute(&mut *tx)
                    .await?;
                }
            }
        }

        let was = prev.map(|(_, a)| a).unwrap_or_else(|| "ok".into());
        if was != new_alert {
            if new_alert == "ok" {
                // Healthy again: close the entries that described the problem.
                sqlx::query(
                    "UPDATE error_events SET resolved_at = now() WHERE site_id = $1 AND resolved_at IS NULL
                        AND code IN ('hardware.device_offline', 'hardware.device_degraded') AND details->>'device' = $2",
                )
                .bind(n.site_id)
                .bind(&key)
                .execute(&mut *tx)
                .await?;
            }
            changes.push(Change { state: new_alert, name: name.clone(), key: key.clone(), kind: kind.clone(), ip: opt(&d.ip, 60), why });
        }
    }
    tx.commit().await?;

    for c in changes {
        let (code, sev, msg) = match c.state {
            "offline" => ("hardware.device_offline", severity_for(&c.kind, "offline"), format!("{} ({}) is not answering at {}", c.name, c.kind, site_name)),
            "degraded" => ("hardware.device_degraded", "warning", format!("{} ({}) needs attention at {}: {}", c.name, c.kind, site_name, c.why.join(" "))),
            _ => ("hardware.device_recovered", "info", format!("{} ({}) is healthy again at {}", c.name, c.kind, site_name)),
        };
        record(
            &st.pool,
            n.tenant,
            NewEvent::new("hardware", sev, code, msg)
                .site(n.site_id)
                .details(json!({ "device": c.key, "name": c.name, "kind": c.kind, "ip": c.ip, "site": site_name, "reasons": c.why })),
        )
        .await;
    }
    Ok(json!({ "ok": true, "stored": stored }))
}

/// Background pass (run with the offline-node watcher): devices the agent has stopped mentioning while the
/// agent itself is alive, and old data.
pub async fn sweep(pool: &PgPool, tenant: Uuid) -> Result<()> {
    let mut tx = tenant_tx(pool, tenant).await?;
    let gone = sqlx::query(&format!(
        "UPDATE net_devices d SET alert_state = 'offline'
          WHERE d.alert_state <> 'offline' AND d.reported_at < now() - interval '{STALE_MINUTES} minutes'
            AND EXISTS (SELECT 1 FROM metrics m WHERE m.site_id = d.site_id AND m.metric = 'agent_up' AND m.ts > now() - interval '{STALE_MINUTES} minutes')
        RETURNING d.dev_key, d.name, d.kind, d.site_id, d.ip, d.reported_at::text AS last, (SELECT s.name FROM sites s WHERE s.id = d.site_id) AS site"
    ))
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM net_device_samples WHERE ts < now() - make_interval(days => $1)").bind(SAMPLE_DAYS).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM net_devices WHERE reported_at < now() - interval '30 days'").execute(&mut *tx).await?;
    tx.commit().await?;
    for r in gone {
        let (name, kind, site): (String, String, String) = (r.try_get("name")?, r.try_get("kind")?, r.try_get("site")?);
        let sev = if matches!(kind.as_str(), "router" | "ap" | "cpe" | "controller") { "error" } else { "warning" };
        record(
            pool,
            tenant,
            NewEvent::new("hardware", sev, "hardware.device_offline", format!("{name} ({kind}) has dropped off the list at {site}"))
                .site(r.try_get("site_id")?)
                .details(json!({
                    "device": r.try_get::<String, _>("dev_key")?, "name": name, "kind": kind, "ip": r.try_get::<Option<String>, _>("ip")?, "site": site,
                    "last_reported": r.try_get::<String, _>("last")?,
                    "reasons": ["The site agent is running but has stopped reporting this device. Its collector may be failing (see the site agent entries) or the device was removed."],
                })),
        )
        .await;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Staff views (every role)
// ---------------------------------------------------------------------------

/// Status as shown to people: a device nobody has heard about lately is offline whatever it last said.
const EFFECTIVE_STATUS: &str = "CASE WHEN d.reported_at < now() - interval '3 minutes' OR NOT d.reachable THEN 'offline' ELSE d.status END";
const EFFECTIVE_REASONS: &str =
    "CASE WHEN d.reported_at < now() - interval '3 minutes' THEN to_jsonb(ARRAY['Nothing has been reported about this device for over 3 minutes. Check the site agent and the device.']) ELSE d.reasons END";

pub async fn list(st: &AppState, tenant: &str, h: &HeaderMap, status: Option<&str>, kind: Option<&str>, q: Option<&str>) -> Result<Value> {
    let a = authenticate(st, tenant, h, Perm::Devices).await?;
    if status.is_some_and(|s| !["online", "degraded", "offline"].contains(&s)) || kind.is_some_and(|k| !KINDS.contains(&k)) {
        return Err(AppError::BadRequest("unknown status or kind".into()));
    }
    let pat = format!("%{}%", q.unwrap_or("").trim().replace(['%', '_'], ""));
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let devices: Value = sqlx::query_scalar(&format!(
        "SELECT COALESCE(jsonb_agg(to_jsonb(t) - 'kind_order' ORDER BY t.site, t.kind_order, t.name), '[]'::jsonb) FROM (
           SELECT d.id, s.name AS site, s.slug AS site_slug, d.name, d.kind, CASE d.kind WHEN 'controller' THEN 0 WHEN 'router' THEN 1 WHEN 'cpe' THEN 2 WHEN 'ap' THEN 3 WHEN 'switch' THEN 4 ELSE 5 END AS kind_order,
                  d.model, d.mac, d.ip, d.firmware, d.peer, {EFFECTIVE_STATUS} AS status, {EFFECTIVE_REASONS} AS reasons,
                  d.uptime_s, d.cpu_pct, d.mem_pct, d.signal_dbm, d.noise_dbm, d.link_quality_pct, d.tx_mbps, d.rx_mbps, d.channel, d.util_pct, d.latency_ms, d.loss_pct,
                  d.clients_count, d.last_seen_at, d.reported_at
             FROM net_devices d JOIN sites s ON s.id = d.site_id
            WHERE ($1::text IS NULL OR {EFFECTIVE_STATUS} = $1) AND ($2::text IS NULL OR d.kind = $2)
              AND (d.name ILIKE $3 OR COALESCE(d.ip, '') ILIKE $3 OR COALESCE(d.mac, '') ILIKE $3 OR COALESCE(d.model, '') ILIKE $3)) t"
    ))
    .bind(status)
    .bind(kind)
    .bind(pat)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    let arr = devices.as_array().cloned().unwrap_or_default();
    let count = |s: &str| arr.iter().filter(|d| d["status"] == s).count();
    let clients: i64 = arr.iter().filter(|d| d["kind"] == "ap" || d["kind"] == "router").map(|d| d["clients_count"].as_i64().unwrap_or(0)).sum();
    Ok(
        json!({ "summary": { "online": count("online"), "degraded": count("degraded"), "offline": count("offline"), "total": arr.len(), "clients_on_aps": clients }, "devices": devices }),
    )
}

pub async fn detail(st: &AppState, tenant: &str, h: &HeaderMap, id: Uuid) -> Result<Value> {
    let a = authenticate(st, tenant, h, Perm::Devices).await?;
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    let device: Option<Value> = sqlx::query_scalar(&format!(
        "SELECT to_jsonb(t) FROM (
           SELECT d.id, s.name AS site, d.dev_key, d.name, d.kind, d.model, d.mac, d.ip, d.firmware, d.peer, {EFFECTIVE_STATUS} AS status, {EFFECTIVE_REASONS} AS reasons,
                  d.uptime_s, d.cpu_pct, d.mem_pct, d.signal_dbm, d.noise_dbm, d.link_quality_pct, d.tx_mbps, d.rx_mbps, d.channel, d.util_pct, d.latency_ms, d.loss_pct,
                  d.clients_count, d.details, d.first_seen_at, d.last_seen_at, d.reported_at, d.site_id
             FROM net_devices d JOIN sites s ON s.id = d.site_id WHERE d.id = $1) t"
    ))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let device = device.ok_or(AppError::NotFound)?;
    let clients: Value = sqlx::query_scalar(
        "SELECT COALESCE(jsonb_agg(t ORDER BY t.signal_dbm DESC NULLS LAST), '[]'::jsonb) FROM (
           SELECT c.mac::text AS mac, c.hostname, c.ip, c.ssid, c.signal_dbm, c.connected_s, c.rx_bytes, c.tx_bytes,
                  dv.os_family, dv.device_class, a.display_name AS member_name, COALESCE(a.phone, ac.phone) AS customer_phone, p.name AS plan
             FROM net_clients c
             LEFT JOIN devices dv ON dv.mac = c.mac
             LEFT JOIN LATERAL (SELECT e.* FROM device_bindings b JOIN entitlements e ON e.id = b.entitlement_id
                                 WHERE b.device_id = dv.id AND b.released_at IS NULL AND e.status IN ('pending', 'active') ORDER BY e.granted_at DESC LIMIT 1) e ON true
             LEFT JOIN accounts a ON a.id = e.account_id
             LEFT JOIN access_codes ac ON ac.id = e.access_code_id
             LEFT JOIN plans p ON p.id = e.plan_id
            WHERE c.device_id = $1) t",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    let samples: Value = sqlx::query_scalar(
        "SELECT COALESCE(jsonb_agg(t ORDER BY t.ts), '[]'::jsonb) FROM (
           SELECT date_bin('10 minutes', ts, TIMESTAMPTZ '2000-01-01') AS ts, round(avg(signal_dbm)::numeric, 1) AS signal_dbm, round(avg(cpu_pct)::numeric, 1) AS cpu_pct,
                  round(avg(mem_pct)::numeric, 1) AS mem_pct, round(avg(util_pct)::numeric, 1) AS util_pct, round(avg(latency_ms)::numeric, 1) AS latency_ms,
                  round(max(loss_pct)::numeric, 1) AS loss_pct, round(avg(clients)::numeric, 1) AS clients
             FROM net_device_samples WHERE device_id = $1 AND ts > now() - interval '24 hours' GROUP BY 1) t",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    let events: Value = sqlx::query_scalar(
        "SELECT COALESCE(jsonb_agg(t ORDER BY t.id DESC), '[]'::jsonb) FROM (
           SELECT id, ts, severity, code, message, resolved_at FROM error_events
            WHERE site_id = $1 AND details->>'device' = $2 ORDER BY id DESC LIMIT 15) t",
    )
    .bind(device["site_id"].as_str().and_then(|s| Uuid::parse_str(s).ok()))
    .bind(device["dev_key"].as_str())
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(json!({ "device": device, "clients": clients, "samples": samples, "events": events }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(kind: &str) -> DeviceIn {
        DeviceIn { key: "k".into(), name: "n".into(), kind: Some(kind.into()), ..Default::default() }
    }

    #[test]
    fn a_quiet_healthy_device_is_online() {
        let x = DeviceIn {
            cpu_pct: Some(20.0),
            mem_pct: Some(40.0),
            signal_dbm: Some(-55.0),
            link_quality_pct: Some(95.0),
            latency_ms: Some(3.0),
            loss_pct: Some(0.0),
            ..d("cpe")
        };
        assert_eq!(assess(&x), ("online", vec![]));
    }

    #[test]
    fn an_unreachable_device_is_offline_with_a_reason() {
        let (s, why) = assess(&DeviceIn { reachable: Some(false), ..d("ap") });
        assert_eq!(s, "offline");
        assert!(why[0].contains("cannot reach"));
    }

    #[test]
    fn a_weak_radio_link_is_degraded_but_only_for_radios() {
        let weak = DeviceIn { signal_dbm: Some(-78.0), ..d("cpe") };
        let (s, why) = assess(&weak);
        assert_eq!(s, "degraded");
        assert!(why[0].contains("-78 dBm"));
        // an AP's signal figure is not a link measurement, so it is never judged by this rule
        assert_eq!(assess(&DeviceIn { signal_dbm: Some(-78.0), ..d("ap") }).0, "online");
        assert_eq!(assess(&DeviceIn { signal_dbm: Some(-75.0), ..d("cpe") }).0, "degraded", "the limit itself counts");
        assert_eq!(assess(&DeviceIn { signal_dbm: Some(-74.0), ..d("cpe") }).0, "online");
    }

    #[test]
    fn load_loss_and_latency_each_give_their_own_reason() {
        let x = DeviceIn {
            cpu_pct: Some(95.0),
            mem_pct: Some(93.0),
            util_pct: Some(85.0),
            loss_pct: Some(6.0),
            latency_ms: Some(250.0),
            link_quality_pct: Some(40.0),
            ..d("router")
        };
        let (s, why) = assess(&x);
        assert_eq!((s, why.len()), ("degraded", 6));
        assert_eq!(
            assess(&DeviceIn { cpu_pct: Some(89.0), mem_pct: Some(91.0), util_pct: Some(79.0), loss_pct: Some(4.0), latency_ms: Some(199.0), ..d("router") }).0,
            "online"
        );
    }

    #[test]
    fn severity_follows_how_much_the_device_matters() {
        assert_eq!(severity_for("cpe", "offline"), "critical");
        assert_eq!(severity_for("ap", "offline"), "critical");
        assert_eq!(severity_for("switch", "offline"), "error");
        assert_eq!(severity_for("ap", "degraded"), "warning");
    }

    #[test]
    fn numbers_are_kept_sane() {
        assert_eq!(num(Some(250.0), 0.0, 100.0), Some(100.0));
        assert_eq!(num(Some(f32::NAN), 0.0, 100.0), None);
        assert_eq!(num(None, 0.0, 100.0), None);
    }
}
