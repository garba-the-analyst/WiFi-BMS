//! Time-based one-time passwords (RFC 6238) for admin two-factor sign-in.
//!
//! Compatible with Google Authenticator, Microsoft Authenticator, Authy and
//! 1Password: 6 digits, 30-second steps, HMAC-SHA1.

use hmac::{Hmac, Mac};
use rand::RngCore;
use sha1::Sha1;

const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
pub const STEP_SECS: u64 = 30;
pub const DIGITS: u32 = 6;

/// A fresh 160-bit secret.
pub fn new_secret() -> Vec<u8> {
    let mut s = vec![0u8; 20];
    rand::rngs::OsRng.fill_bytes(&mut s);
    s
}

/// RFC 4648 base32 without padding: the form authenticator apps expect.
pub fn base32_encode(data: &[u8]) -> String {
    let mut out = String::new();
    let (mut buffer, mut bits) = (0u32, 0u32);
    for &b in data {
        buffer = (buffer << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            out.push(ALPHABET[((buffer >> (bits - 5)) & 31) as usize] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

pub fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let (mut buffer, mut bits) = (0u32, 0u32);
    let mut out = Vec::new();
    for c in s.chars().filter(|c| *c != '=' && !c.is_whitespace()) {
        let v = ALPHABET.iter().position(|&a| a as char == c.to_ascii_uppercase())? as u32;
        buffer = (buffer << 5) | v;
        bits += 5;
        if bits >= 8 {
            out.push((buffer >> (bits - 8)) as u8);
            bits -= 8;
        }
    }
    Some(out)
}

/// The code for one time step.
pub fn code_at(secret: &[u8], unix_secs: u64, digits: u32) -> String {
    let counter = (unix_secs / STEP_SECS).to_be_bytes();
    let mut mac = Hmac::<Sha1>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(&counter);
    let h = mac.finalize().into_bytes();
    let offset = (h[19] & 0x0f) as usize;
    let bin = (u32::from(h[offset] & 0x7f) << 24) | (u32::from(h[offset + 1]) << 16) | (u32::from(h[offset + 2]) << 8) | u32::from(h[offset + 3]);
    format!("{:0width$}", bin % 10u32.pow(digits), width = digits as usize)
}

/// Accept the current step and one either side (clock drift of up to 30 s).
/// Returns the step number that matched, so the caller can refuse to accept
/// the same step twice (replay).
pub fn verify(secret: &[u8], unix_secs: u64, submitted: &str) -> Option<u64> {
    let submitted = submitted.trim();
    if submitted.len() != DIGITS as usize || !submitted.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let step = unix_secs / STEP_SECS;
    for s in [step.saturating_sub(1), step, step + 1] {
        let expected = code_at(secret, s * STEP_SECS, DIGITS);
        if crate::auth::ct_eq(&expected, submitted) {
            return Some(s);
        }
    }
    None
}

/// The `otpauth://` link an authenticator app scans as a QR code.
pub fn provisioning_uri(issuer: &str, account: &str, secret: &[u8]) -> String {
    let enc = |s: &str| s.replace('%', "%25").replace(' ', "%20").replace(':', "%3A").replace('@', "%40");
    format!(
        "otpauth://totp/{}:{}?secret={}&issuer={}&algorithm=SHA1&digits={}&period={}",
        enc(issuer),
        enc(account),
        base32_encode(secret),
        enc(issuer),
        DIGITS,
        STEP_SECS
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 6238 appendix B: secret "12345678901234567890", SHA-1, 8 digits.
    const RFC_SECRET: &[u8] = b"12345678901234567890";

    #[test]
    fn matches_the_rfc_6238_test_vectors() {
        assert_eq!(code_at(RFC_SECRET, 59, 8), "94287082");
        assert_eq!(code_at(RFC_SECRET, 1_111_111_109, 8), "07081804");
        assert_eq!(code_at(RFC_SECRET, 1_234_567_890, 8), "89005924");
        assert_eq!(code_at(RFC_SECRET, 20_000_000_000, 8), "65353130");
    }

    #[test]
    fn six_digit_code_is_the_tail_of_the_eight_digit_one() {
        assert_eq!(code_at(RFC_SECRET, 59, 6), "287082");
    }

    #[test]
    fn base32_round_trips_and_matches_the_rfc_4648_vectors() {
        assert_eq!(base32_encode(b"foobar"), "MZXW6YTBOI");
        assert_eq!(base32_encode(b"fooba"), "MZXW6YTB");
        assert_eq!(base32_decode("MZXW6YTBOI").unwrap(), b"foobar");
        assert_eq!(base32_decode("mzxw 6ytb oi").unwrap(), b"foobar", "case and spaces are tolerated");
        assert!(base32_decode("not*base32").is_none());
        let s = new_secret();
        assert_eq!(base32_decode(&base32_encode(&s)).unwrap(), s);
    }

    #[test]
    fn verify_accepts_one_step_of_drift_and_no_more() {
        let t = 1_700_000_010;
        let now_code = code_at(RFC_SECRET, t, 6);
        assert_eq!(verify(RFC_SECRET, t, &now_code), Some(t / 30));
        assert!(verify(RFC_SECRET, t + 30, &now_code).is_some(), "one step late");
        assert!(verify(RFC_SECRET, t - 30, &now_code).is_some(), "one step early");
        assert!(verify(RFC_SECRET, t + 90, &now_code).is_none(), "three steps away");
        assert!(verify(RFC_SECRET, t, "12345").is_none(), "wrong length");
        assert!(verify(RFC_SECRET, t, "12a456").is_none(), "not digits");
    }

    #[test]
    fn provisioning_uri_has_what_apps_need() {
        let u = provisioning_uri("Minna Wi-Fi", "owner@example.com", RFC_SECRET);
        assert!(u.starts_with("otpauth://totp/Minna%20Wi-Fi:owner%40example.com?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"));
        assert!(u.contains("digits=6") && u.contains("period=30"));
    }
}
