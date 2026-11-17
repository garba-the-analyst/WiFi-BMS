//! Credentials: random tokens, password hashing, rate limiting, client address.
//!
//! * Tokens (member, admin, node) are 32 random bytes, shown once. Only their
//!   SHA-256 is stored, so a database leak does not hand out working sessions.
//! * Passwords use Argon2id. Hashing is deliberately slow, so callers run it
//!   with `tokio::task::spawn_blocking`.
//! * Unknown users cost the same time as wrong passwords (`dummy_verify`) so
//!   response time does not reveal which phone numbers are registered.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::http::HeaderMap;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::error::{AppError, Result};

/// A new random token such as `mt_Zk3...`. The prefix says what it is for
/// (`mt_` member, `at_` admin, `nt_` node), which helps when reading logs and
/// lets secret scanners spot a leaked one.
pub fn new_token(prefix: &str) -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    format!("{prefix}_{}", B64URL.encode(bytes))
}

pub fn sha256_hex(input: &str) -> String {
    hex::encode(Sha256::digest(input.as_bytes()))
}

/// Constant-time comparison of two strings.
pub fn ct_eq(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

pub const MIN_PASSWORD_LEN: usize = 8;
pub const MAX_PASSWORD_LEN: usize = 128;

/// Reject passwords that are too short, too long, or the phone number itself.
pub fn check_password_policy(password: &str, phone: &str) -> Result<()> {
    let len = password.chars().count();
    if len < MIN_PASSWORD_LEN {
        return Err(AppError::BadRequest(format!("password must be at least {MIN_PASSWORD_LEN} characters")));
    }
    if len > MAX_PASSWORD_LEN {
        return Err(AppError::BadRequest("password is too long".into()));
    }
    let digits: String = phone.chars().filter(char::is_ascii_digit).collect();
    if password.chars().filter(char::is_ascii_digit).collect::<String>() == digits || password.chars().all(|c| c == password.chars().next().unwrap()) {
        return Err(AppError::BadRequest("choose a password that is harder to guess".into()));
    }
    Ok(())
}

/// Argon2id with the crate's recommended defaults.
pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default().hash_password(password.as_bytes(), &salt).map(|h| h.to_string()).map_err(|e| AppError::Internal(format!("password hashing failed: {e}")))
}

pub fn verify_password(hash: &str, password: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok(),
        Err(_) => false,
    }
}

/// Spend the same time as a real check, for accounts that do not exist.
pub fn dummy_verify(password: &str) {
    // Hashed once on first use with the same parameters as real passwords, so
    // the cost is identical and cannot silently drift from a hard-coded string.
    static DUMMY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let hash = DUMMY.get_or_init(|| hash_password("timing-equaliser-not-a-real-password").expect("hashing works"));
    let _ = verify_password(hash, password);
}

/// A simple fixed-window limiter kept in memory.
///
/// Good enough for one API process and for blunting guessing. If you run
/// several API replicas, each has its own counts, so the real limit is the
/// per-process limit times the replicas; put a shared limiter (Caddy, Redis)
/// in front if that matters.
pub struct RateLimiter {
    inner: Mutex<HashMap<String, (Instant, u32)>>,
}

impl RateLimiter {
    pub fn new() -> Self {
        RateLimiter { inner: Mutex::new(HashMap::new()) }
    }

    /// Count one hit for `key`. Returns `Err(RateLimited)` once `max` hits
    /// have happened inside `window`.
    pub fn hit(&self, key: &str, max: u32, window: Duration) -> Result<()> {
        let now = Instant::now();
        let mut map = self.inner.lock().expect("limiter lock");
        if map.len() > 20_000 {
            map.retain(|_, (start, _)| now.duration_since(*start) < window);
        }
        let entry = map.entry(key.to_string()).or_insert((now, 0));
        if now.duration_since(entry.0) >= window {
            *entry = (now, 0);
        }
        entry.1 += 1;
        if entry.1 > max {
            Err(AppError::RateLimited)
        } else {
            Ok(())
        }
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// The caller's address. Behind our own reverse proxy (Caddy) the real address
/// is the first entry of `X-Forwarded-For`; without a proxy that header is
/// attacker-controlled and must be ignored, hence `trust_proxy`.
pub fn client_ip(headers: &HeaderMap, trust_proxy: bool) -> Option<String> {
    if !trust_proxy {
        return None;
    }
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|s| s.trim().to_string())
        .filter(|s| s.parse::<std::net::IpAddr>().is_ok())
}

/// `Authorization: Bearer <token>` → the token.
pub fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers.get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).map(str::trim).filter(|t| !t.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_prefixed_long_and_unique() {
        let a = new_token("mt");
        let b = new_token("mt");
        assert!(a.starts_with("mt_"));
        assert!(a.len() > 40);
        assert_ne!(a, b);
    }

    #[test]
    fn hashes_are_stable_sha256() {
        assert_eq!(sha256_hex("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "abcd"));
    }

    #[test]
    fn passwords_hash_and_verify() {
        let h = hash_password("correct horse battery").unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_password(&h, "correct horse battery"));
        assert!(!verify_password(&h, "wrong"));
        assert!(!verify_password("not a hash", "x"));
        assert_ne!(h, hash_password("correct horse battery").unwrap(), "salted");
        let start = std::time::Instant::now();
        dummy_verify("anything");
        assert!(start.elapsed() > Duration::from_millis(1), "the dummy check must do real Argon2 work");
    }

    #[test]
    fn password_policy() {
        assert!(check_password_policy("short", "+2348012345678").is_err());
        assert!(check_password_policy("aaaaaaaaaa", "+2348012345678").is_err());
        assert!(check_password_policy("2348012345678", "+2348012345678").is_err());
        assert!(check_password_policy(&"x1".repeat(70), "+2348012345678").is_err());
        assert!(check_password_policy("River-Stone-42", "+2348012345678").is_ok());
    }

    #[test]
    fn limiter_blocks_after_max_and_resets() {
        let l = RateLimiter::new();
        for _ in 0..3 {
            assert!(l.hit("k", 3, Duration::from_millis(80)).is_ok());
        }
        assert!(matches!(l.hit("k", 3, Duration::from_millis(80)), Err(AppError::RateLimited)));
        assert!(l.hit("other", 3, Duration::from_millis(80)).is_ok(), "keys are independent");
        std::thread::sleep(Duration::from_millis(100));
        assert!(l.hit("k", 3, Duration::from_millis(80)).is_ok(), "window reset");
    }

    #[test]
    fn forwarded_address_is_used_only_behind_a_trusted_proxy() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "203.0.113.9, 10.0.0.1".parse().unwrap());
        assert_eq!(client_ip(&h, true).as_deref(), Some("203.0.113.9"));
        assert_eq!(client_ip(&h, false), None);
        h.insert("x-forwarded-for", "garbage".parse().unwrap());
        assert_eq!(client_ip(&h, true), None);
    }

    #[test]
    fn bearer_parsing() {
        let mut h = HeaderMap::new();
        assert_eq!(bearer(&h), None);
        h.insert("authorization", "Bearer abc123".parse().unwrap());
        assert_eq!(bearer(&h), Some("abc123"));
        h.insert("authorization", "Basic abc".parse().unwrap());
        assert_eq!(bearer(&h), None);
    }
}
