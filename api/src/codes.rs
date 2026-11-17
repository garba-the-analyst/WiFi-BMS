//! Access codes (guest login) and order references.
//!
//! Access codes use 32 characters with no 0, 1, I or O, so a code read out
//! over the phone or typed from a receipt is hard to get wrong. Eight
//! characters give 32^8, about 1.1 trillion codes, so guessing is hopeless
//! and the portal also rate-limits attempts.

use rand::Rng;

/// Same alphabet as the database CHECK on `access_codes.code`.
pub const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
pub const CODE_LEN: usize = 8;

fn random_string(len: usize) -> String {
    let mut rng = rand::thread_rng();
    (0..len).map(|_| CODE_ALPHABET[rng.gen_range(0..CODE_ALPHABET.len())] as char).collect()
}

/// A fresh guest access code, e.g. `K7M2QX4R`.
pub fn new_access_code() -> String {
    random_string(CODE_LEN)
}

/// An unguessable order reference sent to Paystack, e.g. `ord_K7M2QX4RP4RT8WNZ5HXC`.
/// The reference is also the capability to read the order's status, so it is long.
pub fn new_order_reference() -> String {
    format!("ord_{}", random_string(20))
}

/// Show a code as `K7M2 QX4R` for people. Anything not 8 characters is returned unchanged.
pub fn pretty_code(code: &str) -> String {
    if code.len() == CODE_LEN {
        format!("{} {}", &code[..4], &code[4..])
    } else {
        code.to_string()
    }
}

/// Normalise what a person typed: remove spaces and dashes, upper-case.
pub fn normalise_code(input: &str) -> String {
    input.chars().filter(|c| !c.is_whitespace() && *c != '-').flat_map(|c| c.to_uppercase()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_use_only_the_safe_alphabet() {
        for _ in 0..500 {
            let c = new_access_code();
            assert_eq!(c.len(), CODE_LEN);
            assert!(c.bytes().all(|b| CODE_ALPHABET.contains(&b)), "bad char in {c}");
            assert!(!c.contains(['0', '1', 'I', 'O']));
        }
    }

    #[test]
    fn codes_are_not_repeated_in_a_small_sample() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..2000 {
            assert!(seen.insert(new_access_code()));
        }
    }

    #[test]
    fn references_are_long_and_prefixed() {
        let r = new_order_reference();
        assert!(r.starts_with("ord_"));
        assert_eq!(r.len(), 24);
    }

    #[test]
    fn typed_codes_are_normalised() {
        assert_eq!(normalise_code(" k7m2-qx4r "), "K7M2QX4R");
        assert_eq!(pretty_code("K7M2QX4R"), "K7M2 QX4R");
    }
}
