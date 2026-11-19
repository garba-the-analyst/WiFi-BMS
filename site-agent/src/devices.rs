//! Reads the site's network hardware and reports its health to the platform's Devices page.
//!
//! What it can read, and how (each is optional; list only what the site has in `devices.json`):
//!
//! | source     | for                         | gives                                                                            |
//! |------------|-----------------------------|----------------------------------------------------------------------------------|
//! | `ping`     | any device with an address  | up/down, latency, packet loss                                                    |
//! | `snmp`     | CPE radios, switches, APs   | ping plus uptime and any number the device exposes by SNMP (signal, noise, CCQ…) |
//! | `routeros` | the MikroTik router         | CPU, memory, uptime, version, and every hotspot client                           |
//! | `omada`    | Omada controller (OC200 or the free Software Controller) | every access point it manages: health, channel, and the clients on each          |
//!
//! Every round (default 60 s) all sources are read in parallel. A source that fails does not stop the
//! others: its failure becomes one `hardware.collector_failed` entry in the error log, and its devices simply
//! drop off this round's report (the platform then shows them as not reporting). Nothing here can disturb
//! customer logins: it runs on its own task and only reads.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::UdpSocket;
use tokio::task::JoinSet;

use crate::wire::ErrorEvent;

// ---------------------------------------------------------------------------
// What is sent to the platform (matches `api/src/netmon.rs`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct ClientReport {
    pub mac: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal_dbm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connected_s: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rx_bytes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tx_bytes: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct DeviceReport {
    pub key: String,
    pub name: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub firmware: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer: Option<String>,
    pub reachable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime_s: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mem_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal_dbm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub noise_dbm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_quality_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tx_mbps: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rx_mbps: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub util_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loss_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clients: Option<Vec<ClientReport>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

// ---------------------------------------------------------------------------
// Settings file (`devices.json`)
// ---------------------------------------------------------------------------

fn default_interval() -> u64 {
    60
}
fn default_community() -> String {
    "public".into()
}
fn default_snmp_port() -> u16 {
    161
}
fn default_kind() -> String {
    "other".into()
}

#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
pub struct DevicesConfig {
    #[serde(default = "default_interval")]
    pub interval_s: u64,
    #[serde(default)]
    pub ping: Vec<PingTarget>,
    #[serde(default)]
    pub snmp: Vec<SnmpTarget>,
    #[serde(default)]
    pub routeros: Option<RouterOsTarget>,
    #[serde(default)]
    pub omada: Option<OmadaTarget>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct PingTarget {
    pub name: String,
    pub ip: String,
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub peer: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct SnmpTarget {
    pub name: String,
    pub ip: String,
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub peer: Option<String>,
    #[serde(default = "default_community")]
    pub community: String,
    #[serde(default = "default_snmp_port")]
    pub port: u16,
    /// Number to read for each metric name: `signal_dbm`, `noise_dbm`, `link_quality_pct`, `cpu_pct`, `mem_pct`,
    /// `util_pct`, `tx_mbps`, `rx_mbps`. Find the OIDs with `snmpwalk` on the device (see docs/site-setup.md).
    #[serde(default)]
    pub metrics: HashMap<String, String>,
    /// Multiply the raw number by this (for example 0.1 when the device reports tenths of a dBm).
    #[serde(default)]
    pub scale: HashMap<String, f64>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct RouterOsTarget {
    pub name: String,
    /// `https://192.168.88.1`
    pub url: String,
    pub user: String,
    pub pass: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct OmadaTarget {
    /// `https://192.168.88.5:8043` (OC200 default port)
    pub url: String,
    pub omada_id: String,
    /// Omada site id (shown in the controller's URL), not this platform's site.
    pub site_id: String,
    pub client_id: String,
    pub client_secret: String,
}

impl DevicesConfig {
    pub fn load(path: &str) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
        let cfg: DevicesConfig = serde_json::from_str(&text).map_err(|e| format!("{path} is not valid: {e}"))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(10..=3600).contains(&self.interval_s) {
            return Err("interval_s must be between 10 and 3600".into());
        }
        let kinds = ["router", "ap", "cpe", "switch", "controller", "other"];
        for (name, ip, kind) in self.ping.iter().map(|p| (&p.name, &p.ip, &p.kind)).chain(self.snmp.iter().map(|s| (&s.name, &s.ip, &s.kind))) {
            if name.trim().is_empty() {
                return Err("every device needs a name".into());
            }
            if !safe_host(ip) {
                return Err(format!("{name}: '{ip}' is not a valid address"));
            }
            if !kinds.contains(&kind.as_str()) {
                return Err(format!("{name}: kind must be one of {}", kinds.join(", ")));
            }
        }
        for s in &self.snmp {
            for oid in s.metrics.values() {
                if parse_oid(oid).is_none() {
                    return Err(format!("{}: '{oid}' is not a valid OID (use digits and dots, e.g. 1.3.6.1.2.1.1.3.0)", s.name));
                }
            }
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.ping.is_empty() && self.snmp.is_empty() && self.routeros.is_none() && self.omada.is_none()
    }
}

/// An address or host name, never anything that could be read as an option by `ping`.
pub fn safe_host(h: &str) -> bool {
    !h.is_empty() && h.len() <= 100 && !h.starts_with('-') && h.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'))
}

// ---------------------------------------------------------------------------
// Ping
// ---------------------------------------------------------------------------

/// Loss and average round trip from `ping -q` output. `None` when the output is not ping's.
pub fn parse_ping(out: &str) -> Option<(f64, Option<f64>)> {
    let loss = out.split(',').find(|p| p.contains("packet loss"))?.trim().split('%').next()?.trim().parse::<f64>().ok()?;
    let avg = out
        .lines()
        .find(|l| l.contains("min/avg/max"))
        .and_then(|l| l.split('=').nth(1))
        .and_then(|v| v.trim().split('/').nth(1))
        .and_then(|v| v.trim().parse::<f64>().ok());
    Some((loss, avg))
}

pub async fn ping(host: &str) -> Option<(f64, Option<f64>)> {
    if !safe_host(host) {
        return None;
    }
    let out = tokio::time::timeout(Duration::from_secs(12), tokio::process::Command::new("ping").args(["-c", "3", "-W", "2", "-q", host]).output())
        .await
        .ok()?
        .ok()?;
    parse_ping(&String::from_utf8_lossy(&out.stdout))
}

fn base_report(name: &str, kind: &str, model: &Option<String>, ip: &str, peer: &Option<String>) -> DeviceReport {
    DeviceReport {
        key: format!("{kind}:{name}"),
        name: name.to_string(),
        kind: kind.to_string(),
        model: model.clone(),
        ip: Some(ip.to_string()),
        peer: peer.clone(),
        reachable: true,
        ..Default::default()
    }
}

pub async fn collect_ping(t: &PingTarget) -> DeviceReport {
    let mut r = base_report(&t.name, &t.kind, &t.model, &t.ip, &t.peer);
    match ping(&t.ip).await {
        Some((loss, avg)) if loss < 100.0 => {
            r.loss_pct = Some(loss);
            r.latency_ms = avg;
        }
        _ => r.reachable = false,
    }
    r
}

// ---------------------------------------------------------------------------
// SNMP v2c (just GET: that is all a health check needs)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum SnmpValue {
    Int(i64),
    Text(String),
    Missing,
}

impl SnmpValue {
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            SnmpValue::Int(i) => Some(*i as f64),
            SnmpValue::Text(t) => t.trim().parse().ok(),
            SnmpValue::Missing => None,
        }
    }
}

pub fn parse_oid(s: &str) -> Option<Vec<u32>> {
    let parts: Vec<u32> = s.trim_start_matches('.').split('.').map(|p| p.parse().ok()).collect::<Option<_>>()?;
    (parts.len() >= 2 && parts[0] <= 2).then_some(parts)
}

fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let n = content.len();
    if n < 128 {
        out.push(n as u8);
    } else {
        let b = n.to_be_bytes();
        let b: Vec<u8> = b.iter().copied().skip_while(|x| *x == 0).collect();
        out.push(0x80 | b.len() as u8);
        out.extend(b);
    }
    out.extend_from_slice(content);
    out
}

fn enc_int(v: i64) -> Vec<u8> {
    let mut bytes = v.to_be_bytes().to_vec();
    while bytes.len() > 1 && ((bytes[0] == 0 && bytes[1] & 0x80 == 0) || (bytes[0] == 0xff && bytes[1] & 0x80 != 0)) {
        bytes.remove(0);
    }
    tlv(0x02, &bytes)
}

fn enc_oid(oid: &[u32]) -> Vec<u8> {
    let mut body = vec![(oid[0] * 40 + oid[1]) as u8];
    for &arc in &oid[2..] {
        let mut stack = vec![(arc & 0x7f) as u8];
        let mut a = arc >> 7;
        while a > 0 {
            stack.push(0x80 | (a & 0x7f) as u8);
            a >>= 7;
        }
        stack.reverse();
        body.extend(stack);
    }
    tlv(0x06, &body)
}

/// A whole SNMP v2c message. `pdu_tag` is 0xA0 for a GET request, 0xA2 for a response.
pub fn build_message(pdu_tag: u8, community: &str, request_id: i64, varbinds: &[(Vec<u32>, Option<SnmpValue>)]) -> Vec<u8> {
    let mut binds = Vec::new();
    for (oid, val) in varbinds {
        let v = match val {
            None | Some(SnmpValue::Missing) => tlv(0x05, &[]),
            Some(SnmpValue::Int(i)) => enc_int(*i),
            Some(SnmpValue::Text(t)) => tlv(0x04, t.as_bytes()),
        };
        let mut one = enc_oid(oid);
        one.extend(v);
        binds.extend(tlv(0x30, &one));
    }
    let mut pdu = enc_int(request_id);
    pdu.extend(enc_int(0));
    pdu.extend(enc_int(0));
    pdu.extend(tlv(0x30, &binds));
    let mut msg = enc_int(1); // version 2c
    msg.extend(tlv(0x04, community.as_bytes()));
    msg.extend(tlv(pdu_tag, &pdu));
    tlv(0x30, &msg)
}

fn read_tlv(b: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let tag = *b.first()?;
    let l = *b.get(1)? as usize;
    let (len, off) = if l < 128 {
        (l, 2)
    } else {
        let n = l & 0x7f;
        if n == 0 || n > 4 {
            return None;
        }
        (b.get(2..2 + n)?.iter().fold(0usize, |a, x| (a << 8) | *x as usize), 2 + n)
    };
    let content = b.get(off..off.checked_add(len)?)?;
    Some((tag, content, &b[off + len..]))
}

fn dec_int(c: &[u8]) -> Option<i64> {
    if c.is_empty() || c.len() > 9 {
        return None;
    }
    let mut v: i64 = if c[0] & 0x80 != 0 { -1 } else { 0 };
    for b in c {
        v = (v << 8) | *b as i64;
    }
    Some(v)
}

fn dec_oid(c: &[u8]) -> Option<Vec<u32>> {
    let first = *c.first()? as u32;
    let mut out = vec![first / 40, first % 40];
    let mut acc: u32 = 0;
    for b in &c[1..] {
        acc = acc.checked_mul(128)?.checked_add((b & 0x7f) as u32)?;
        if b & 0x80 == 0 {
            out.push(acc);
            acc = 0;
        }
    }
    Some(out)
}

/// Request id and the value for each OID in an SNMP v2c response. Errors and 'no such object' become `Missing`.
pub type Varbinds = Vec<(Vec<u32>, SnmpValue)>;

pub fn parse_response(b: &[u8]) -> Option<(i64, Varbinds)> {
    let (t, msg, _) = read_tlv(b)?;
    if t != 0x30 {
        return None;
    }
    let (_, _version, rest) = read_tlv(msg)?;
    let (_, _community, rest) = read_tlv(rest)?;
    let (pdu_tag, pdu, _) = read_tlv(rest)?;
    if pdu_tag != 0xA2 && pdu_tag != 0xA0 {
        return None;
    }
    let (_, id, rest) = read_tlv(pdu)?;
    let (_, err, rest) = read_tlv(rest)?;
    let (_, _idx, rest) = read_tlv(rest)?;
    let (_, mut binds, _) = read_tlv(rest)?;
    let errored = dec_int(err)? != 0;
    let mut out = Vec::new();
    while !binds.is_empty() {
        let (_, one, next) = read_tlv(binds)?;
        let (_, oid, rest) = read_tlv(one)?;
        let (vt, val, _) = read_tlv(rest)?;
        let v = match vt {
            _ if errored => SnmpValue::Missing,
            0x02 | 0x41 | 0x42 | 0x43 | 0x46 => SnmpValue::Int(if vt == 0x02 { dec_int(val)? } else { val.iter().fold(0i64, |a, x| (a << 8) | *x as i64) }),
            0x04 => SnmpValue::Text(String::from_utf8_lossy(val).into_owned()),
            _ => SnmpValue::Missing,
        };
        out.push((dec_oid(oid)?, v));
        binds = next;
    }
    Some((dec_int(id)?, out))
}

/// Ask a device for several numbers at once. One retry; two seconds each.
pub async fn snmp_get(addr: SocketAddr, community: &str, oids: &[Vec<u32>]) -> Result<Varbinds, String> {
    let sock = UdpSocket::bind(if addr.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }).await.map_err(|e| e.to_string())?;
    sock.connect(addr).await.map_err(|e| e.to_string())?;
    let id = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(1) & 0x3fff_ffff) as i64 + 1;
    let req = build_message(0xA0, community, id, &oids.iter().map(|o| (o.clone(), None)).collect::<Vec<_>>());
    let mut buf = vec![0u8; 4096];
    for _ in 0..2 {
        sock.send(&req).await.map_err(|e| e.to_string())?;
        if let Ok(Ok(n)) = tokio::time::timeout(Duration::from_secs(2), sock.recv(&mut buf)).await {
            if let Some((rid, vals)) = parse_response(&buf[..n]) {
                if rid == id {
                    return Ok(vals);
                }
            }
        }
    }
    Err("no SNMP answer (check the address, the community string, and that SNMP is switched on in the device)".into())
}

const SYS_UPTIME: &str = "1.3.6.1.2.1.1.3.0";
const SYS_DESCR: &str = "1.3.6.1.2.1.1.1.0";

pub async fn collect_snmp(t: &SnmpTarget) -> DeviceReport {
    let mut r = base_report(&t.name, &t.kind, &t.model, &t.ip, &t.peer);
    let pinged = ping(&t.ip).await;
    if let Some((loss, avg)) = pinged {
        r.loss_pct = Some(loss);
        r.latency_ms = avg;
    }
    let addr: Option<SocketAddr> = format!("{}:{}", t.ip, t.port).parse().ok().or_else(|| format!("[{}]:{}", t.ip, t.port).parse().ok());
    let mut names: Vec<&String> = t.metrics.keys().collect();
    names.sort();
    let mut oids = vec![parse_oid(SYS_UPTIME).unwrap(), parse_oid(SYS_DESCR).unwrap()];
    oids.extend(names.iter().filter_map(|n| parse_oid(&t.metrics[*n])));
    let answered = match addr {
        Some(a) => snmp_get(a, &t.community, &oids).await.ok(),
        None => None, // a host name rather than an address: ping still works
    };
    match answered {
        Some(vals) => {
            fill_from_snmp(&mut r, t, &names, &vals);
        }
        None => r.reachable = pinged.is_some_and(|(loss, _)| loss < 100.0),
    }
    r
}

/// Put the values an SNMP answer carries onto the report. Pure, so it is tested without a network.
pub fn fill_from_snmp(r: &mut DeviceReport, t: &SnmpTarget, names: &[&String], vals: &[(Vec<u32>, SnmpValue)]) {
    let get = |oid: &str| parse_oid(oid).and_then(|o| vals.iter().find(|(k, _)| *k == o).map(|(_, v)| v.clone()));
    r.uptime_s = get(SYS_UPTIME).and_then(|v| v.as_f64()).map(|t| (t / 100.0) as i64);
    if let Some(SnmpValue::Text(d)) = get(SYS_DESCR) {
        r.details = Some(json!({ "sys_descr": d.chars().take(200).collect::<String>() }));
    }
    for n in names {
        let Some(v) = get(&t.metrics[*n]).and_then(|v| v.as_f64()) else { continue };
        let v = v * t.scale.get(*n).copied().unwrap_or(1.0);
        match n.as_str() {
            "signal_dbm" => r.signal_dbm = Some(v),
            "noise_dbm" => r.noise_dbm = Some(v),
            "link_quality_pct" => r.link_quality_pct = Some(v),
            "cpu_pct" => r.cpu_pct = Some(v),
            "mem_pct" => r.mem_pct = Some(v),
            "util_pct" => r.util_pct = Some(v),
            "tx_mbps" => r.tx_mbps = Some(v),
            "rx_mbps" => r.rx_mbps = Some(v),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// MikroTik RouterOS (REST, v7)
// ---------------------------------------------------------------------------

/// RouterOS sends numbers as strings. Accept either.
fn num(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

/// `1w2d3h4m5s` -> seconds.
pub fn parse_ros_duration(s: &str) -> Option<i64> {
    if s.is_empty() {
        return None;
    }
    let (mut total, mut cur) = (0i64, String::new());
    for c in s.chars() {
        if c.is_ascii_digit() {
            cur.push(c);
        } else {
            let n: i64 = cur.parse().ok()?;
            cur.clear();
            total += n * match c {
                'w' => 604_800,
                'd' => 86_400,
                'h' => 3_600,
                'm' => 60,
                's' => 1,
                _ => return None,
            };
        }
    }
    cur.is_empty().then_some(total)
}

pub fn router_report(t: &RouterOsTarget, resource: &Value, active: &Value) -> DeviceReport {
    let mut r = DeviceReport { key: format!("router:{}", t.name), name: t.name.clone(), kind: "router".into(), reachable: true, ..Default::default() };
    let host = t.url.split("//").nth(1).unwrap_or(&t.url).split([':', '/']).next().unwrap_or("");
    r.ip = Some(host.to_string());
    r.model = resource.get("board-name").and_then(|v| v.as_str()).map(str::to_string);
    r.firmware = resource.get("version").and_then(|v| v.as_str()).map(|v| format!("RouterOS {v}"));
    r.uptime_s = resource.get("uptime").and_then(|v| v.as_str()).and_then(parse_ros_duration);
    r.cpu_pct = resource.get("cpu-load").and_then(num);
    if let (Some(free), Some(total)) = (resource.get("free-memory").and_then(num), resource.get("total-memory").and_then(num)) {
        if total > 0.0 {
            r.mem_pct = Some(((total - free) / total * 100.0).clamp(0.0, 100.0));
        }
    }
    let clients: Vec<ClientReport> = active
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| {
                    Some(ClientReport {
                        mac: c.get("mac-address")?.as_str()?.to_string(),
                        ip: c.get("address").and_then(|v| v.as_str()).map(str::to_string),
                        hostname: c.get("user").and_then(|v| v.as_str()).map(str::to_string),
                        connected_s: c.get("uptime").and_then(|v| v.as_str()).and_then(parse_ros_duration),
                        rx_bytes: c.get("bytes-out").and_then(num).map(|v| v as i64), // router's "out" is what the customer received
                        tx_bytes: c.get("bytes-in").and_then(num).map(|v| v as i64),
                        ..Default::default()
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    r.clients = Some(clients);
    r
}

pub async fn collect_routeros(t: &RouterOsTarget) -> Result<DeviceReport, String> {
    // The router's own certificate is self-signed; it is only ever reached on the site's private network.
    let client = reqwest::Client::builder().danger_accept_invalid_certs(true).timeout(Duration::from_secs(5)).build().map_err(|e| e.to_string())?;
    let get = |path: &str| {
        let url = format!("{}/rest/{}", t.url.trim_end_matches('/'), path);
        let req = client.get(url).basic_auth(&t.user, Some(&t.pass));
        async move {
            let resp = req.send().await.map_err(|e| format!("cannot reach the router: {e}"))?;
            if resp.status().as_u16() == 401 {
                return Err("the router refused the user name or password".to_string());
            }
            if !resp.status().is_success() {
                return Err(format!("the router answered {} for /{}", resp.status(), "rest"));
            }
            resp.json::<Value>().await.map_err(|e| format!("unreadable answer: {e}"))
        }
    };
    let resource = get("system/resource").await?;
    let active = get("ip/hotspot/active").await.unwrap_or_else(|_| json!([]));
    Ok(router_report(t, &resource, &active))
}

// ---------------------------------------------------------------------------
// TP-Link Omada controller (OC200) Open API
// ---------------------------------------------------------------------------
//
// Written from TP-Link's Omada Open API documentation and tolerant of naming differences between controller
// versions (it accepts several spellings of each field). It has been tested against recorded-style JSON, not a
// live controller: expect to check the first real answer (see docs/site-setup.md).

fn first<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| v.get(*k)).filter(|x| !x.is_null())
}
fn first_str(v: &Value, keys: &[&str]) -> Option<String> {
    first(v, keys).and_then(|x| x.as_str()).filter(|s| !s.is_empty()).map(str::to_string)
}
fn first_num(v: &Value, keys: &[&str]) -> Option<f64> {
    first(v, keys).and_then(num)
}

pub fn omada_devices(v: &Value) -> Vec<DeviceReport> {
    let list = v.pointer("/result/data").or_else(|| v.pointer("/result")).and_then(|x| x.as_array()).cloned().unwrap_or_default();
    list.iter()
        .filter_map(|d| {
            let mac = first_str(d, &["mac"])?;
            let kind = match first_str(d, &["type", "deviceType"]).unwrap_or_default().to_ascii_lowercase().as_str() {
                "ap" => "ap",
                "gateway" | "router" => "router",
                "switch" => "switch",
                _ => "other",
            };
            let connected = match first(d, &["status", "statusCategory"]) {
                Some(Value::Number(n)) => n.as_i64() == Some(1),
                Some(Value::String(s)) => s.eq_ignore_ascii_case("connected") || s == "1",
                _ => true,
            };
            Some(DeviceReport {
                key: mac.to_ascii_lowercase().replace('-', ":"),
                name: first_str(d, &["name"]).unwrap_or_else(|| mac.clone()),
                kind: kind.into(),
                model: first_str(d, &["model", "showModel"]),
                mac: Some(mac),
                ip: first_str(d, &["ip", "ipAddress"]),
                firmware: first_str(d, &["firmwareVersion", "version"]),
                reachable: connected,
                uptime_s: first_num(d, &["uptimeLong", "uptimeSeconds"]).map(|x| x as i64),
                cpu_pct: first_num(d, &["cpuUtil", "cpuUsage"]),
                mem_pct: first_num(d, &["memUtil", "memUsage"]),
                util_pct: first_num(d, &["channelUtil", "radioUtil"]),
                channel: first_str(d, &["channel"]),
                clients: (kind == "ap").then(Vec::new),
                ..Default::default()
            })
        })
        .collect()
}

/// Attach each client to the access point it is connected through.
pub fn attach_omada_clients(devices: &mut [DeviceReport], v: &Value) {
    let list = v.pointer("/result/data").or_else(|| v.pointer("/result")).and_then(|x| x.as_array()).cloned().unwrap_or_default();
    for c in list {
        if c.get("active").and_then(|a| a.as_bool()) == Some(false) {
            continue;
        }
        let (Some(mac), Some(ap)) = (first_str(&c, &["mac"]), first_str(&c, &["apMac", "connectDevMac"])) else { continue };
        let ap = ap.to_ascii_lowercase().replace('-', ":");
        if let Some(d) = devices.iter_mut().find(|d| d.key == ap) {
            let rssi = first_num(&c, &["rssi", "signalLevel"]).filter(|x| *x < 0.0);
            d.clients.get_or_insert_with(Vec::new).push(ClientReport {
                mac,
                hostname: first_str(&c, &["name", "hostName"]),
                ip: first_str(&c, &["ip"]),
                ssid: first_str(&c, &["ssid"]),
                signal_dbm: rssi,
                connected_s: first_num(&c, &["uptime"]).map(|x| x as i64),
                rx_bytes: first_num(&c, &["trafficDown"]).map(|x| x as i64),
                tx_bytes: first_num(&c, &["trafficUp"]).map(|x| x as i64),
            });
        }
    }
}

pub async fn collect_omada(t: &OmadaTarget) -> Result<Vec<DeviceReport>, String> {
    let client = reqwest::Client::builder().danger_accept_invalid_certs(true).timeout(Duration::from_secs(8)).build().map_err(|e| e.to_string())?;
    let base = t.url.trim_end_matches('/');
    let tok: Value = client
        .post(format!("{base}/openapi/authorize/token?grant_type=client_credentials"))
        .json(&json!({ "omadacId": t.omada_id, "client_id": t.client_id, "client_secret": t.client_secret }))
        .send()
        .await
        .map_err(|e| format!("cannot reach the Omada controller: {e}"))?
        .json()
        .await
        .map_err(|e| format!("unreadable sign-in answer from the controller: {e}"))?;
    let token = tok.pointer("/result/accessToken").and_then(|x| x.as_str()).ok_or_else(|| {
        format!(
            "the controller refused the sign-in ({}). Check client_id, client_secret and omada_id",
            tok.get("msg").and_then(|m| m.as_str()).unwrap_or("no reason given")
        )
    })?;
    let get = |what: &str| {
        let url = format!("{base}/openapi/v1/{}/sites/{}/{what}?page=1&pageSize=200", t.omada_id, t.site_id);
        let req = client.get(url).header("Authorization", format!("AccessToken={token}"));
        async move { req.send().await.map_err(|e| e.to_string())?.json::<Value>().await.map_err(|e| e.to_string()) }
    };
    let devices = get("devices").await.map_err(|e| format!("could not list the access points: {e}"))?;
    if devices.get("errorCode").and_then(|e| e.as_i64()).unwrap_or(0) != 0 {
        return Err(format!("the controller said: {}", devices.get("msg").and_then(|m| m.as_str()).unwrap_or("error")));
    }
    let mut out = omada_devices(&devices);
    if let Ok(clients) = get("clients").await {
        attach_omada_clients(&mut out, &clients);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// One round
// ---------------------------------------------------------------------------

/// Read every configured source in parallel. Failures of a whole source come back as error events.
pub async fn collect_all(cfg: &DevicesConfig) -> (Vec<DeviceReport>, Vec<ErrorEvent>) {
    enum Done {
        Devices(Vec<DeviceReport>),
        Failed(&'static str, String),
    }
    let mut set = JoinSet::new();
    for t in cfg.ping.clone() {
        set.spawn(async move { Done::Devices(vec![collect_ping(&t).await]) });
    }
    for t in cfg.snmp.clone() {
        set.spawn(async move { Done::Devices(vec![collect_snmp(&t).await]) });
    }
    if let Some(t) = cfg.routeros.clone() {
        set.spawn(async move {
            match collect_routeros(&t).await {
                Ok(d) => Done::Devices(vec![d]),
                Err(e) => Done::Failed("the MikroTik router", e),
            }
        });
    }
    if let Some(t) = cfg.omada.clone() {
        set.spawn(async move {
            match collect_omada(&t).await {
                Ok(d) => Done::Devices(d),
                Err(e) => Done::Failed("the Omada controller", e),
            }
        });
    }
    let (mut reports, mut problems) = (Vec::new(), Vec::new());
    while let Some(done) = set.join_next().await {
        match done {
            Ok(Done::Devices(d)) => reports.extend(d),
            Ok(Done::Failed(what, why)) => problems.push(ErrorEvent {
                source: "hardware",
                severity: "error",
                code: "hardware.collector_failed",
                message: format!("Could not read {what}: {why}"),
                details: json!({ "collector": what, "reason": why }),
            }),
            Err(e) => tracing::error!(error = %e, "a device collector crashed"),
        }
    }
    reports.sort_by(|a, b| a.key.cmp(&b.key));
    (reports, problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_output_is_read() {
        let out = "PING 192.168.88.10 (192.168.88.10) 56(84) bytes of data.\n\n--- 192.168.88.10 ping statistics ---\n3 packets transmitted, 3 received, 0% packet loss, time 2003ms\nrtt min/avg/max/mdev = 1.100/2.200/3.300/0.400 ms\n";
        assert_eq!(parse_ping(out), Some((0.0, Some(2.2))));
        let dead = "3 packets transmitted, 0 received, 100% packet loss, time 2051ms\n";
        assert_eq!(parse_ping(dead), Some((100.0, None)));
        assert_eq!(parse_ping("ping: unknown host"), None);
    }

    #[test]
    fn only_plain_addresses_may_reach_the_ping_command() {
        assert!(safe_host("192.168.88.10") && safe_host("cpe-1.lan") && safe_host("fe80::1"));
        assert!(!safe_host("-f") && !safe_host("a b") && !safe_host("x;rm") && !safe_host(""));
    }

    #[test]
    fn config_is_checked_before_it_is_used() {
        let ok: DevicesConfig = serde_json::from_str(
            r#"{ "ping": [{ "name": "Hub", "ip": "192.168.88.10", "kind": "cpe" }],
                 "snmp": [{ "name": "R", "ip": "10.0.0.2", "kind": "cpe", "metrics": { "signal_dbm": "1.3.6.1.4.1.11863.1.1.1" }, "scale": { "signal_dbm": 0.1 } }] }"#,
        )
        .unwrap();
        assert_eq!(ok.interval_s, 60);
        assert!(ok.validate().is_ok());
        assert_eq!(ok.snmp[0].community, "public");
        let bad_ip: DevicesConfig = serde_json::from_str(r#"{ "ping": [{ "name": "x", "ip": "-c1 evil" }] }"#).unwrap();
        assert!(bad_ip.validate().unwrap_err().contains("not a valid address"));
        let bad_oid: DevicesConfig = serde_json::from_str(r#"{ "snmp": [{ "name": "x", "ip": "1.2.3.4", "metrics": { "signal_dbm": "abc" } }] }"#).unwrap();
        assert!(bad_oid.validate().unwrap_err().contains("not a valid OID"));
        let bad_kind: DevicesConfig = serde_json::from_str(r#"{ "ping": [{ "name": "x", "ip": "1.2.3.4", "kind": "toaster" }] }"#).unwrap();
        assert!(bad_kind.validate().is_err());
        assert!(DevicesConfig { interval_s: 1, ..Default::default() }.validate().is_err());
        assert!(DevicesConfig::default().is_empty() || DevicesConfig { interval_s: 60, ..Default::default() }.is_empty());
    }

    #[test]
    fn snmp_messages_round_trip_including_big_and_negative_numbers() {
        let oid = parse_oid("1.3.6.1.4.1.11863.1.1.1").unwrap();
        let vals = vec![
            (oid.clone(), Some(SnmpValue::Int(-62))),
            (parse_oid("1.3.6.1.2.1.1.3.0").unwrap(), Some(SnmpValue::Int(8_640_000))),
            (parse_oid("1.3.6.1.2.1.1.1.0").unwrap(), Some(SnmpValue::Text("TP-Link CPE710".into()))),
        ];
        let msg = build_message(0xA2, "public", 42, &vals);
        let (id, got) = parse_response(&msg).unwrap();
        assert_eq!(id, 42);
        assert_eq!(got[0], (oid, SnmpValue::Int(-62)));
        assert_eq!(got[1].1, SnmpValue::Int(8_640_000));
        assert_eq!(got[2].1, SnmpValue::Text("TP-Link CPE710".into()));
        // a long message (length > 127) and garbage
        let many: Vec<_> = (0..30).map(|i| (parse_oid(&format!("1.3.6.1.2.1.2.2.1.10.{i}")).unwrap(), Some(SnmpValue::Int(i * 1000)))).collect();
        assert_eq!(parse_response(&build_message(0xA2, "public", 7, &many)).unwrap().1.len(), 30);
        assert_eq!(parse_response(&[0x30, 0x05, 1, 2]), None);
        assert_eq!(parse_response(b""), None);
    }

    #[test]
    fn counters_gauges_and_timeticks_are_numbers() {
        // hand-built value with tag 0x42 (Gauge32) = 300
        let mut bind = enc_oid(&parse_oid("1.3.6.1.2.1.1.3.0").unwrap());
        bind.extend(tlv(0x42, &[0x01, 0x2c]));
        let mut pdu = enc_int(5);
        pdu.extend(enc_int(0));
        pdu.extend(enc_int(0));
        pdu.extend(tlv(0x30, &tlv(0x30, &bind)));
        let mut msg = enc_int(1);
        msg.extend(tlv(0x04, b"public"));
        msg.extend(tlv(0xA2, &pdu));
        assert_eq!(parse_response(&tlv(0x30, &msg)).unwrap().1[0].1, SnmpValue::Int(300));
    }

    #[tokio::test]
    async fn an_snmp_device_is_read_over_udp_and_scaled() {
        // a tiny fake radio that answers every GET from a table
        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = sock.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            let (n, from) = sock.recv_from(&mut buf).await.unwrap();
            let (id, asked) = parse_response(&buf[..n]).unwrap();
            let answers: Vec<_> = asked
                .iter()
                .map(|(o, _)| {
                    let v = match o.last() {
                        Some(0) if o[o.len() - 2] == 3 => SnmpValue::Int(360_000), // sysUpTime: 3600 s
                        Some(0) => SnmpValue::Text("TP-Link CPE510".into()),
                        _ => SnmpValue::Int(-683), // tenths of a dBm
                    };
                    (o.clone(), Some(v))
                })
                .collect();
            sock.send_to(&build_message(0xA2, "public", id, &answers), from).await.unwrap();
        });
        let t = SnmpTarget {
            name: "Remote radio".into(),
            ip: "127.0.0.1".into(),
            kind: "cpe".into(),
            model: None,
            peer: None,
            community: "public".into(),
            port: addr.port(),
            metrics: HashMap::from([("signal_dbm".to_string(), "1.3.6.1.4.1.9.9.1".to_string())]),
            scale: HashMap::from([("signal_dbm".to_string(), 0.1)]),
        };
        let oids = vec![parse_oid(SYS_UPTIME).unwrap(), parse_oid(SYS_DESCR).unwrap(), parse_oid("1.3.6.1.4.1.9.9.1").unwrap()];
        let vals = snmp_get(addr, "public", &oids).await.unwrap();
        let mut r = base_report(&t.name, &t.kind, &t.model, &t.ip, &t.peer);
        let names: Vec<&String> = t.metrics.keys().collect();
        fill_from_snmp(&mut r, &t, &names, &vals);
        assert_eq!(r.uptime_s, Some(3600));
        assert!((r.signal_dbm.unwrap() - (-68.3)).abs() < 1e-9, "{:?}", r.signal_dbm);
        assert_eq!(r.details.unwrap()["sys_descr"], "TP-Link CPE510");
    }

    #[tokio::test]
    async fn a_silent_snmp_device_fails_cleanly() {
        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap(); // listens, never answers
        let err = snmp_get(sock.local_addr().unwrap(), "public", &[parse_oid(SYS_UPTIME).unwrap()]).await.unwrap_err();
        assert!(err.contains("no SNMP answer"));
    }

    #[test]
    fn routeros_durations_and_resources_are_read() {
        assert_eq!(parse_ros_duration("1w2d3h4m5s"), Some(604_800 + 2 * 86_400 + 3 * 3600 + 4 * 60 + 5));
        assert_eq!(parse_ros_duration("45s"), Some(45));
        assert_eq!(parse_ros_duration("3x"), None);
        assert_eq!(parse_ros_duration("12"), None);
        let t = RouterOsTarget { name: "Hub router".into(), url: "https://192.168.88.1".into(), user: "u".into(), pass: "p".into() };
        let resource = json!({ "board-name": "RB5009UG+S+", "version": "7.15 (stable)", "uptime": "1d2h", "cpu-load": "7", "free-memory": "250000000", "total-memory": "1000000000" });
        let active = json!([
            { "mac-address": "AA:BB:CC:00:11:33", "address": "10.5.50.20", "user": "K7QM-2WXD", "uptime": "10m5s", "bytes-in": "1000", "bytes-out": "9000" },
            { "address": "10.5.50.21" }
        ]);
        let r = router_report(&t, &resource, &active);
        assert_eq!(
            (r.kind.as_str(), r.model.as_deref(), r.ip.as_deref(), r.uptime_s, r.cpu_pct),
            ("router", Some("RB5009UG+S+"), Some("192.168.88.1"), Some(93_600), Some(7.0))
        );
        assert_eq!(r.mem_pct, Some(75.0));
        let c = r.clients.unwrap();
        assert_eq!(c.len(), 1, "a row without a MAC address is skipped");
        assert_eq!((c[0].connected_s, c[0].rx_bytes, c[0].tx_bytes), (Some(605), Some(9000), Some(1000)));
    }

    #[test]
    fn omada_devices_and_clients_are_read_and_matched() {
        let devices = json!({ "errorCode": 0, "result": { "data": [
            { "mac": "AA-BB-CC-DD-EE-01", "name": "Market AP 1", "type": "ap", "model": "EAP225-Outdoor(US)", "ip": "192.168.88.21", "status": 1, "cpuUtil": 31, "memUtil": 52, "uptimeLong": 7200, "firmwareVersion": "1.2.3" },
            { "mac": "AA-BB-CC-DD-EE-02", "name": "Market AP 2", "type": "ap", "status": 0 },
            { "mac": "AA-BB-CC-DD-EE-03", "name": "Gateway", "type": "gateway", "status": 1 } ] } });
        let mut d = omada_devices(&devices);
        assert_eq!(d.len(), 3);
        assert_eq!(
            (d[0].key.as_str(), d[0].kind.as_str(), d[0].reachable, d[0].cpu_pct, d[0].uptime_s),
            ("aa:bb:cc:dd:ee:01", "ap", true, Some(31.0), Some(7200))
        );
        assert!(!d[1].reachable, "status 0 = disconnected");
        assert_eq!(d[2].kind, "router");
        let clients = json!({ "result": { "data": [
            { "mac": "11-22-33-44-55-66", "name": "tecno", "ip": "10.5.50.20", "apMac": "AA-BB-CC-DD-EE-01", "ssid": "Waste2Light", "rssi": -61, "uptime": 300, "trafficDown": 5000, "trafficUp": 700, "active": true },
            { "mac": "11-22-33-44-55-77", "apMac": "AA-BB-CC-DD-EE-01", "active": false },
            { "mac": "11-22-33-44-55-88", "apMac": "FF-FF-FF-FF-FF-FF" } ] } });
        attach_omada_clients(&mut d, &clients);
        let c = d[0].clients.as_ref().unwrap();
        assert_eq!((c.len(), c[0].signal_dbm, c[0].rx_bytes, c[0].ssid.as_deref()), (1, Some(-61.0), Some(5000), Some("Waste2Light")));
        assert_eq!(d[1].clients.as_ref().unwrap().len(), 0, "an AP with nobody on it reports an empty list, so stale clients are cleared");
        assert!(d[2].clients.is_none(), "a gateway's clients are not an AP's");
    }

    #[test]
    fn the_report_sent_to_the_platform_leaves_out_what_is_unknown() {
        let r = DeviceReport { key: "k".into(), name: "n".into(), kind: "cpe".into(), reachable: true, signal_dbm: Some(-60.0), ..Default::default() };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v, json!({ "key": "k", "name": "n", "kind": "cpe", "reachable": true, "signal_dbm": -60.0 }));
    }
}
