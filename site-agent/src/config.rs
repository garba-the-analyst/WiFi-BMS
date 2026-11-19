//! Settings, all from environment variables (see `deploy/site-agent.env.example`).

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Central platform, e.g. `https://api.example.com`.
    pub central_url: String,
    /// `nt_...` token created for this site.
    pub node_token: String,
    /// Where FreeRADIUS reaches the agent. Keep it on loopback.
    pub listen: String,
    /// Shared secret FreeRADIUS sends in `X-Agent-Key`, so other local programs cannot call the agent.
    pub local_key: String,
    /// Router's CoA/Disconnect address, e.g. `192.168.88.1:3799`.
    pub coa_addr: String,
    pub coa_secret: String,
    pub radclient_bin: String,
    /// Optional RouterOS REST address for DHCP hints, e.g. `https://192.168.88.1`.
    pub router_rest_url: Option<String>,
    pub router_user: Option<String>,
    pub router_pass: Option<String>,
    pub spool_path: String,
    /// Optional `devices.json`: which radios, access points and routers to watch (see `docs/site-setup.md`).
    pub devices_file: Option<String>,
}

#[derive(Debug, thiserror::Error)]
#[error("missing or invalid setting: {0}")]
pub struct ConfigError(pub String);

impl Config {
    /// Build from a lookup function, so tests do not touch the real environment.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let need = |k: &str| get(k).filter(|v| !v.trim().is_empty()).ok_or_else(|| ConfigError(k.to_string()));
        let central_url = need("CENTRAL_URL")?;
        if !central_url.starts_with("https://") && !central_url.starts_with("http://localhost") && !central_url.starts_with("http://127.") {
            return Err(ConfigError("CENTRAL_URL must use https:// (the node token is a credential)".into()));
        }
        let node_token = need("NODE_TOKEN")?;
        if !node_token.starts_with("nt_") {
            return Err(ConfigError("NODE_TOKEN must start with nt_".into()));
        }
        let local_key = need("LOCAL_KEY")?;
        if local_key.len() < 16 {
            return Err(ConfigError("LOCAL_KEY must be at least 16 characters".into()));
        }
        let listen = get("LISTEN").unwrap_or_else(|| "127.0.0.1:8081".into());
        if !listen.starts_with("127.") && !listen.starts_with("[::1]") && !listen.starts_with("localhost") {
            return Err(ConfigError("LISTEN must be a loopback address; FreeRADIUS runs on the same machine".into()));
        }
        Ok(Config {
            central_url: central_url.trim_end_matches('/').to_string(),
            node_token,
            listen,
            local_key,
            coa_addr: need("COA_ADDR")?,
            coa_secret: need("COA_SECRET")?,
            radclient_bin: get("RADCLIENT_BIN").unwrap_or_else(|| "radclient".into()),
            router_rest_url: get("ROUTER_REST_URL").filter(|v| !v.is_empty()),
            router_user: get("ROUTER_USER").filter(|v| !v.is_empty()),
            router_pass: get("ROUTER_PASS").filter(|v| !v.is_empty()),
            spool_path: get("SPOOL_PATH").unwrap_or_else(|| "/var/lib/site-agent/spool.jsonl".into()),
            devices_file: get("DEVICES_FILE").filter(|v| !v.is_empty()),
        })
    }

    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn good() -> HashMap<&'static str, &'static str> {
        HashMap::from([
            ("CENTRAL_URL", "https://api.example.com/"),
            ("NODE_TOKEN", "nt_abc"),
            ("LOCAL_KEY", "0123456789abcdef"),
            ("COA_ADDR", "192.168.88.1:3799"),
            ("COA_SECRET", "s3cret"),
        ])
    }

    fn build(m: &HashMap<&'static str, &'static str>) -> Result<Config, ConfigError> {
        Config::from_lookup(|k| m.get(k).map(|v| v.to_string()))
    }

    #[test]
    fn a_complete_environment_builds_with_defaults() {
        let c = build(&good()).unwrap();
        assert_eq!(c.central_url, "https://api.example.com", "trailing slash removed");
        assert_eq!(c.listen, "127.0.0.1:8081");
        assert_eq!(c.radclient_bin, "radclient");
        assert!(c.router_rest_url.is_none());
    }

    #[test]
    fn insecure_or_missing_settings_are_refused() {
        let mut m = good();
        m.insert("CENTRAL_URL", "http://api.example.com");
        assert!(build(&m).is_err(), "plain http to a remote host");
        let mut m = good();
        m.insert("NODE_TOKEN", "mt_member_token");
        assert!(build(&m).is_err());
        let mut m = good();
        m.insert("LOCAL_KEY", "short");
        assert!(build(&m).is_err());
        let mut m = good();
        m.insert("LISTEN", "0.0.0.0:8081");
        assert!(build(&m).is_err(), "must not listen on the network");
        let mut m = good();
        m.remove("COA_SECRET");
        assert!(build(&m).is_err());
        let mut m = good();
        m.insert("CENTRAL_URL", "http://localhost:8080");
        assert!(build(&m).is_ok(), "loopback http is fine for development");
    }
}
