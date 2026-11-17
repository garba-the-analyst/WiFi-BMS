//! Formats session limits as FreeRADIUS reply attributes for a MikroTik
//! Hotspot.
//!
//! Attribute names follow FreeRADIUS's `dictionary.mikrotik`. Check them
//! against your RouterOS and FreeRADIUS versions during the stage-3 test with
//! `radtest`, especially `Mikrotik-Total-Limit-Gigawords`.

use crate::entitlement::SessionLimits;
use crate::plan::SpeedProfile;

/// Format a speed profile for `Mikrotik-Rate-Limit`.
///
/// RouterOS reads the value as `rx/tx` **from the router's side**: `rx` is what
/// the router receives from the client (the client's *upload*) and `tx` is what
/// it sends to the client (the *download*). So the string is `up/down`.
///
/// Without burst: `2000k/5000k`.
/// With burst: `up/down burst-up/burst-down threshold/threshold time/time`.
pub fn mikrotik_rate_limit(speed: &SpeedProfile) -> String {
    match &speed.burst {
        None => format!("{}k/{}k", speed.up_kbps, speed.down_kbps),
        Some(b) => format!(
            "{}k/{}k {}k/{}k {}k/{}k {}/{}",
            speed.up_kbps, speed.down_kbps, b.up_kbps, b.down_kbps, b.threshold_kbps, b.threshold_kbps, b.time_secs, b.time_secs
        ),
    }
}

/// Split a 64-bit byte count into the low 32 bits and the number of whole
/// 4 GiB blocks (the "gigawords"), because RADIUS integers are 32 bits wide.
pub fn split_gigawords(bytes: u64) -> (u32, u32) {
    ((bytes & 0xFFFF_FFFF) as u32, (bytes >> 32) as u32)
}

/// The reply attributes for one session, as `(name, value)` pairs.
pub fn reply_attributes(limits: &SessionLimits) -> Vec<(String, String)> {
    let mut out = vec![
        ("Session-Timeout".to_owned(), limits.session_timeout_secs.to_string()),
        ("Idle-Timeout".to_owned(), limits.idle_timeout_secs.to_string()),
        ("Acct-Interim-Interval".to_owned(), limits.interim_secs.to_string()),
        ("Mikrotik-Rate-Limit".to_owned(), mikrotik_rate_limit(&limits.speed)),
    ];
    if let Some(bytes) = limits.total_limit_bytes {
        let (low, giga) = split_gigawords(bytes);
        out.push(("Mikrotik-Total-Limit".to_owned(), low.to_string()));
        if giga > 0 {
            out.push(("Mikrotik-Total-Limit-Gigawords".to_owned(), giga.to_string()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::Burst;

    fn speed(burst: Option<Burst>) -> SpeedProfile {
        SpeedProfile { down_kbps: 5000, up_kbps: 2000, burst, priority: 8 }
    }

    #[test]
    fn rate_limit_is_upload_then_download() {
        assert_eq!(mikrotik_rate_limit(&speed(None)), "2000k/5000k");
    }

    #[test]
    fn rate_limit_with_burst() {
        let b = Burst { down_kbps: 8000, up_kbps: 4000, threshold_kbps: 1500, time_secs: 10 };
        assert_eq!(mikrotik_rate_limit(&speed(Some(b))), "2000k/5000k 4000k/8000k 1500k/1500k 10/10");
    }

    #[test]
    fn gigawords_split() {
        assert_eq!(split_gigawords(1_000), (1_000, 0));
        assert_eq!(split_gigawords(5 * 1024 * 1024 * 1024), (1_073_741_824, 1));
        assert_eq!(split_gigawords(u64::from(u32::MAX) + 1), (0, 1));
    }
}
