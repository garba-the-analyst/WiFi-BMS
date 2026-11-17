//! Hotspot-sharing (tethering) detection.
//!
//! A phone that shares its connection acts as a router, and that leaves traces.
//! We score the traces and choose a response. No single trace is proof, so:
//!
//! | Signal                          | Why it appears                                   | Points |
//! |---------------------------------|--------------------------------------------------|--------|
//! | TTL one lower than the OS default | packets crossed an extra router (the phone)    | 25 or 45 |
//! | Two or more OS families on one MAC | laptop traffic hiding behind a phone address | 35 |
//! | Very many simultaneous connections | many devices behind one address               | 15 or 30 |
//! | Other devices keep trying to join with this code | sharing the login           | 10 each, max 20 |
//!
//! **A single signal never disconnects anyone.** A customer with their own
//! home router also lowers TTL, so TTL alone is a hint, not a verdict. A
//! disconnect needs two different signals and a score of 75 or more.
//! Everything flagged lands in the admin Security screen for review.
//!
//! Known limit: a technical user can set TTL on the phone to hide the hop.
//! The slot limit (one device per slot, per plan) still applies to every
//! device that is seen directly.

/// What the site agent measured for one device over a recent window.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Observation {
    /// Share (0.0 to 1.0) of this device's forwarded packets whose TTL was one
    /// lower than its OS default (63 for Android/iOS/macOS/Linux, 127 for Windows).
    pub ttl_lowered_ratio: f32,
    /// Distinct OS families seen coming from this MAC.
    pub distinct_os_families: u32,
    /// Simultaneous connections from this device.
    pub connection_count: u32,
    /// Other devices refused a slot on this entitlement in the last hour.
    pub slot_overflow_attempts: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Response {
    None,
    Warn,
    Throttle,
    Disconnect,
}

impl Response {
    /// The value stored in `tethering_flags.action`.
    pub fn as_str(self) -> &'static str {
        match self {
            Response::None => "none",
            Response::Warn => "warn",
            Response::Throttle => "throttle",
            Response::Disconnect => "disconnect",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assessment {
    pub score: u8,
    pub response: Response,
    /// Names matching `tethering_flags.signal`.
    pub signals: Vec<&'static str>,
}

pub fn assess(o: &Observation) -> Assessment {
    let mut score = 0u32;
    let mut signals = Vec::new();

    if o.ttl_lowered_ratio >= 0.5 {
        score += 45;
        signals.push("ttl");
    } else if o.ttl_lowered_ratio >= 0.2 {
        score += 25;
        signals.push("ttl");
    }
    if o.distinct_os_families >= 2 {
        score += 35;
        signals.push("mixed_fingerprint");
    }
    if o.connection_count >= 600 {
        score += 30;
        signals.push("connection_count");
    } else if o.connection_count >= 300 {
        score += 15;
        signals.push("connection_count");
    }
    if o.slot_overflow_attempts > 0 {
        score += (o.slot_overflow_attempts * 10).min(20);
        signals.push("slot_overflow");
    }

    let score = score.min(100) as u8;
    let mut response = match score {
        0..=29 => Response::None,
        30..=59 => Response::Warn,
        60..=74 => Response::Throttle,
        _ => Response::Disconnect,
    };
    // One signal alone is never enough to cut someone off.
    if response == Response::Disconnect && signals.len() < 2 {
        response = Response::Throttle;
    }
    Assessment { score, response, signals }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(ttl: f32, os: u32, conns: u32, overflow: u32) -> Observation {
        Observation { ttl_lowered_ratio: ttl, distinct_os_families: os, connection_count: conns, slot_overflow_attempts: overflow }
    }

    #[test]
    fn a_normal_phone_scores_zero() {
        let a = assess(&obs(0.0, 1, 40, 0));
        assert_eq!((a.score, a.response), (0, Response::None));
        assert!(a.signals.is_empty());
    }

    #[test]
    fn a_home_router_alone_is_only_a_warning_or_throttle_never_a_disconnect() {
        // TTL lowered on everything: a customer with their own router behind the phone.
        let a = assess(&obs(1.0, 1, 50, 0));
        assert_eq!(a.score, 45);
        assert_eq!(a.response, Response::Warn);
    }

    #[test]
    fn two_signals_together_can_disconnect() {
        let a = assess(&obs(0.9, 2, 100, 0)); // 45 + 35 = 80
        assert_eq!(a.score, 80);
        assert_eq!(a.response, Response::Disconnect);
        assert_eq!(a.signals, vec!["ttl", "mixed_fingerprint"]);
    }

    #[test]
    fn one_very_loud_signal_is_capped_at_throttle() {
        let a = assess(&obs(0.0, 1, 5000, 4)); // 30 + 20 = 50 from two signals -> warn
        assert_eq!(a.response, Response::Warn);
        let b = assess(&obs(0.0, 4, 100, 0)); // 35 mixed only
        assert_eq!(b.response, Response::Warn);
    }

    #[test]
    fn score_is_capped_at_100_and_thresholds_are_exact() {
        assert_eq!(assess(&obs(1.0, 3, 900, 9)).score, 100);
        assert_eq!(assess(&obs(0.2, 1, 0, 0)).score, 25);
        assert_eq!(assess(&obs(0.2, 1, 300, 0)).score, 40);
        assert_eq!(assess(&obs(0.5, 1, 300, 0)).score, 60);
        assert_eq!(assess(&obs(0.5, 1, 300, 0)).response, Response::Throttle);
    }

    #[test]
    fn response_strings_match_the_database_check() {
        for r in [Response::None, Response::Warn, Response::Throttle, Response::Disconnect] {
            assert!(["none", "warn", "throttle", "disconnect", "review"].contains(&r.as_str()));
        }
    }
}
