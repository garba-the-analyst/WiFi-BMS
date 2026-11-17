//! Reading what FreeRADIUS sends and shaping what we send back.
//!
//! FreeRADIUS's `rlm_rest` module (with `body = 'json'`) posts every request
//! attribute as `{"User-Name": {"type":"string","value":["tk123"]}, ...}`, and
//! expects replies as `{"reply:Session-Timeout": {"value":["3600"],"op":":="}}`.
//! We also accept plain `{"User-Name": "tk123"}` so the agent can be tested
//! with curl. If your FreeRADIUS version formats these differently, this is
//! the only file to change. Validate on the real server with `radtest`.

use serde_json::{json, Map, Value};

use crate::wire::{AccountingReq, AuthorizeReq};

/// First value of an attribute, as text.
pub fn attr(body: &Value, name: &str) -> Option<String> {
    let v = body.get(name)?;
    let first = match v {
        Value::Object(o) => match o.get("value")? {
            Value::Array(a) => a.first()?.clone(),
            other => other.clone(),
        },
        Value::Array(a) => a.first()?.clone(),
        other => other.clone(),
    };
    match first {
        Value::String(s) => Some(s),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn num(body: &Value, name: &str) -> u64 {
    attr(body, name).and_then(|s| s.parse().ok()).unwrap_or(0)
}

/// Combine the 32-bit octet counter with its "gigawords" (number of times it wrapped past 4 GiB).
pub fn octets(body: &Value, base: &str, giga: &str) -> u64 {
    (num(body, giga) << 32) | (num(body, base) & 0xFFFF_FFFF)
}

pub fn to_authorize(body: &Value) -> Option<AuthorizeReq> {
    Some(AuthorizeReq {
        username: attr(body, "User-Name")?,
        password: attr(body, "User-Password").or_else(|| attr(body, "Cleartext-Password"))?,
        mac: attr(body, "Calling-Station-Id")?,
        dhcp_hostname: None,
        dhcp_vendor_class: None,
    })
}

pub fn to_accounting(body: &Value) -> Option<AccountingReq> {
    let status = match attr(body, "Acct-Status-Type")?.as_str() {
        "Start" | "1" => "start",
        "Interim-Update" | "3" => "interim",
        "Stop" | "2" => "stop",
        _ => return None, // Accounting-On/Off and others are not session events
    };
    Some(AccountingReq {
        status: status.into(),
        acct_session_id: attr(body, "Acct-Session-Id")?,
        username: attr(body, "User-Name")?,
        mac: attr(body, "Calling-Station-Id")?,
        ip: attr(body, "Framed-IP-Address"),
        bytes_in: octets(body, "Acct-Input-Octets", "Acct-Input-Gigawords"),
        bytes_out: octets(body, "Acct-Output-Octets", "Acct-Output-Gigawords"),
        session_secs: num(body, "Acct-Session-Time").min(u64::from(u32::MAX)) as u32,
        ssid: attr(body, "Called-Station-Id"),
        terminate_cause: attr(body, "Acct-Terminate-Cause"),
        observation: None,
    })
}

/// Reply attributes in rlm_rest's JSON form.
pub fn reply_json(attributes: &[(String, String)]) -> Value {
    let mut m = Map::new();
    for (k, v) in attributes {
        m.insert(format!("reply:{k}"), json!({ "value": [v], "op": ":=" }));
    }
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fr(pairs: &[(&str, &str)]) -> Value {
        let mut m = Map::new();
        for (k, v) in pairs {
            m.insert((*k).into(), json!({ "type": "string", "value": [v] }));
        }
        Value::Object(m)
    }

    #[test]
    fn reads_freeradius_attribute_objects_and_plain_values() {
        let b = fr(&[("User-Name", "tk123")]);
        assert_eq!(attr(&b, "User-Name").as_deref(), Some("tk123"));
        assert_eq!(attr(&json!({ "User-Name": "plain" }), "User-Name").as_deref(), Some("plain"));
        assert_eq!(attr(&json!({ "N": { "value": [42] } }), "N").as_deref(), Some("42"));
        assert_eq!(attr(&b, "Missing"), None);
    }

    #[test]
    fn builds_an_authorize_request() {
        let b = fr(&[("User-Name", "tkabc"), ("User-Password", "tp_secret"), ("Calling-Station-Id", "A4-83-E7-12-34-56")]);
        let a = to_authorize(&b).unwrap();
        assert_eq!((a.username.as_str(), a.password.as_str(), a.mac.as_str()), ("tkabc", "tp_secret", "A4-83-E7-12-34-56"));
        assert!(to_authorize(&fr(&[("User-Name", "x")])).is_none(), "password and MAC are required");
    }

    #[test]
    fn gigawords_extend_the_32_bit_counters() {
        let b = fr(&[("Acct-Input-Octets", "100"), ("Acct-Input-Gigawords", "2")]);
        assert_eq!(octets(&b, "Acct-Input-Octets", "Acct-Input-Gigawords"), 2 * 4_294_967_296 + 100);
        assert_eq!(octets(&fr(&[]), "Acct-Input-Octets", "Acct-Input-Gigawords"), 0);
    }

    #[test]
    fn maps_accounting_events() {
        let b = fr(&[
            ("Acct-Status-Type", "Interim-Update"),
            ("Acct-Session-Id", "8100001"),
            ("User-Name", "tkabc"),
            ("Calling-Station-Id", "A4-83-E7-12-34-56"),
            ("Framed-IP-Address", "10.5.50.20"),
            ("Acct-Input-Octets", "1000"),
            ("Acct-Output-Octets", "5000"),
            ("Acct-Session-Time", "60"),
            ("Called-Station-Id", "Minna-WiFi"),
        ]);
        let r = to_accounting(&b).unwrap();
        assert_eq!((r.status.as_str(), r.bytes_in, r.bytes_out, r.session_secs), ("interim", 1000, 5000, 60));
        assert_eq!(r.ssid.as_deref(), Some("Minna-WiFi"));
        assert_eq!(
            to_accounting(&fr(&[("Acct-Status-Type", "Start"), ("Acct-Session-Id", "1"), ("User-Name", "u"), ("Calling-Station-Id", "m")])).unwrap().status,
            "start"
        );
        assert_eq!(
            to_accounting(&fr(&[("Acct-Status-Type", "Stop"), ("Acct-Session-Id", "1"), ("User-Name", "u"), ("Calling-Station-Id", "m")])).unwrap().status,
            "stop"
        );
        assert!(to_accounting(&fr(&[("Acct-Status-Type", "Accounting-On")])).is_none());
    }

    #[test]
    fn replies_use_the_reply_list_and_assign_operator() {
        let v = reply_json(&[("Session-Timeout".into(), "3600".into())]);
        assert_eq!(v["reply:Session-Timeout"], json!({ "value": ["3600"], "op": ":=" }));
    }
}
