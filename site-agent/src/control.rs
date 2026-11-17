//! Acting on the router.
//!
//! * **Disconnect / change limits** use RADIUS Change-of-Authorization (CoA)
//!   and Packet-of-Disconnect, sent with FreeRADIUS's `radclient` to the
//!   router's port 3799. This is the portable, supported way to alter a live
//!   hotspot session. On the router: `/radius incoming set accept=yes port=3799`.
//! * **DHCP hints** (hostname, vendor class) come from the RouterOS REST API,
//!   when configured, so the platform can tell what kind of device connected.
//!
//! Both are behind traits so the agent's logic is tested without a router.
//! The real implementations here must be confirmed on actual hardware.

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Enough to point the router at one live session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRef {
    pub acct_session_id: String,
    pub username: String,
}

#[async_trait]
pub trait RouterControl: Send + Sync {
    /// End the session now.
    async fn disconnect(&self, s: &SessionRef) -> Result<(), String>;
    /// Change the speed and/or the total time limit of a live session.
    /// `timeout_secs` is the router's own meaning: total uptime allowed.
    async fn set_limits(&self, s: &SessionRef, rate_limit: Option<&str>, timeout_secs: Option<u64>) -> Result<(), String>;
}

/// What `radclient` reads on stdin, one attribute per line. Values are quoted
/// and stripped of characters that could break out of the quotes.
pub fn radclient_input(s: &SessionRef, extra: &[(&str, String)]) -> String {
    let clean = |v: &str| v.chars().filter(|c| !matches!(c, '"' | '\\' | '\n' | '\r')).collect::<String>();
    let mut out = format!("Acct-Session-Id = \"{}\"\nUser-Name = \"{}\"\n", clean(&s.acct_session_id), clean(&s.username));
    for (k, v) in extra {
        out.push_str(&format!("{k} = \"{}\"\n", clean(v)));
    }
    out
}

pub struct RadClient {
    pub bin: String,
    pub addr: String,
    pub secret: String,
}

impl RadClient {
    async fn run(&self, packet: &str, input: String) -> Result<(), String> {
        let mut child = Command::new(&self.bin)
            .args(["-t", "3", "-r", "2", &self.addr, packet, &self.secret])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", self.bin))?;
        child.stdin.take().expect("piped stdin").write_all(input.as_bytes()).await.map_err(|e| e.to_string())?;
        let out = child.wait_with_output().await.map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!("radclient {packet} failed: {}", String::from_utf8_lossy(&out.stderr).trim()))
        }
    }
}

#[async_trait]
impl RouterControl for RadClient {
    async fn disconnect(&self, s: &SessionRef) -> Result<(), String> {
        self.run("disconnect", radclient_input(s, &[])).await
    }

    async fn set_limits(&self, s: &SessionRef, rate_limit: Option<&str>, timeout_secs: Option<u64>) -> Result<(), String> {
        let mut extra = Vec::new();
        if let Some(r) = rate_limit {
            extra.push(("Mikrotik-Rate-Limit", r.to_string()));
        }
        if let Some(t) = timeout_secs {
            extra.push(("Session-Timeout", t.to_string()));
        }
        if extra.is_empty() {
            return Ok(());
        }
        self.run("coa", radclient_input(s, &extra)).await
    }
}

/// What the router's DHCP server knows about a device.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DhcpHints {
    pub hostname: Option<String>,
    pub vendor_class: Option<String>,
}

#[async_trait]
pub trait DeviceHints: Send + Sync {
    async fn lookup(&self, mac: &str) -> DhcpHints;
}

pub struct NoHints;

#[async_trait]
impl DeviceHints for NoHints {
    async fn lookup(&self, _mac: &str) -> DhcpHints {
        DhcpHints::default()
    }
}

/// RouterOS v7 REST: `GET /rest/ip/dhcp-server/lease?mac-address=...`.
pub struct RouterOsHints {
    client: reqwest::Client,
    base: String,
    user: String,
    pass: String,
}

impl RouterOsHints {
    pub fn new(base: &str, user: &str, pass: &str) -> Self {
        // The router uses a self-signed certificate on the LAN. We still pin
        // to the router's own address, which only this machine can reach.
        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .expect("HTTP client builds");
        RouterOsHints { client, base: base.trim_end_matches('/').to_string(), user: user.into(), pass: pass.into() }
    }
}

/// Pull the two fields we want out of a lease list.
pub fn hints_from_leases(v: &serde_json::Value) -> DhcpHints {
    let lease = v.as_array().and_then(|a| a.first());
    let get = |k: &str| lease.and_then(|l| l.get(k)).and_then(|x| x.as_str()).filter(|s| !s.is_empty()).map(str::to_string);
    DhcpHints { hostname: get("host-name"), vendor_class: get("class-id") }
}

#[async_trait]
impl DeviceHints for RouterOsHints {
    async fn lookup(&self, mac: &str) -> DhcpHints {
        let url = format!("{}/rest/ip/dhcp-server/lease", self.base);
        let resp = self.client.get(url).query(&[("mac-address", mac.to_ascii_uppercase())]).basic_auth(&self.user, Some(&self.pass)).send().await;
        match resp {
            Ok(r) if r.status().is_success() => r.json().await.map(|v: serde_json::Value| hints_from_leases(&v)).unwrap_or_default(),
            _ => DhcpHints::default(), // hints are a bonus; never block a login on them
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sess() -> SessionRef {
        SessionRef { acct_session_id: "81000001".into(), username: "tkabc".into() }
    }

    #[test]
    fn coa_packet_text_is_exact() {
        let t = radclient_input(&sess(), &[("Mikrotik-Rate-Limit", "250k/500k".into()), ("Session-Timeout", "3600".into())]);
        assert_eq!(t, "Acct-Session-Id = \"81000001\"\nUser-Name = \"tkabc\"\nMikrotik-Rate-Limit = \"250k/500k\"\nSession-Timeout = \"3600\"\n");
    }

    #[test]
    fn hostile_values_cannot_inject_extra_attributes() {
        let s = SessionRef { acct_session_id: "1\"\nSession-Timeout = \"9999999".into(), username: "u\\".into() };
        let t = radclient_input(&s, &[]);
        assert_eq!(t.lines().count(), 2, "a newline in a value must not start a new attribute: {t:?}");
        assert!(!t.contains("\\"));
    }

    #[test]
    fn reads_hints_from_a_lease_list() {
        let v = serde_json::json!([{ "host-name": "Amina-iPhone", "class-id": "", "address": "10.5.50.20" }]);
        assert_eq!(hints_from_leases(&v), DhcpHints { hostname: Some("Amina-iPhone".into()), vendor_class: None });
        let v = serde_json::json!([{ "class-id": "android-dhcp-14" }]);
        assert_eq!(hints_from_leases(&v).vendor_class.as_deref(), Some("android-dhcp-14"));
        assert_eq!(hints_from_leases(&serde_json::json!([])), DhcpHints::default());
        assert_eq!(hints_from_leases(&serde_json::json!({"error": 404})), DhcpHints::default());
    }

    #[tokio::test]
    async fn a_missing_radclient_is_reported_not_panicked() {
        let r = RadClient { bin: "/nonexistent/radclient".into(), addr: "127.0.0.1:3799".into(), secret: "x".into() };
        let err = r.disconnect(&sess()).await.unwrap_err();
        assert!(err.contains("cannot start"));
    }
}
