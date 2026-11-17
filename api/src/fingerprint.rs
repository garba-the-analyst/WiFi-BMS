//! What kind of device is this, and what is it running?
//!
//! Three layers, strongest first (see the architecture doc, section 7):
//! 1. the browser's User-Agent, seen when the customer opens the portal;
//! 2. what the device says in DHCP (hostname and vendor class);
//! 3. the shape of its MAC address.
//!
//! Honest limits: all three can be changed by a determined user, and since
//! 2020 phones use a different random MAC per network, so the MAC proves
//! little. We therefore report a **confidence** with every answer and never
//! use it alone for a harsh action.

/// What we worked out about one device.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeviceInfo {
    pub os_family: Option<String>,
    pub os_version: Option<String>,
    /// phone, tablet, laptop, desktop, tv, console, iot, unknown
    pub device_class: String,
    pub mac_randomised: bool,
    /// 0 to 100.
    pub confidence: u8,
}

/// Raw clues. Any of them may be missing.
#[derive(Debug, Clone, Default)]
pub struct Clues<'a> {
    pub mac: Option<&'a str>,
    pub user_agent: Option<&'a str>,
    pub dhcp_hostname: Option<&'a str>,
    pub dhcp_vendor_class: Option<&'a str>,
}

/// A "locally administered" MAC has bit 1 of the first octet set, which shows
/// as the second hex digit being 2, 6, A or E. Phones use these when
/// privacy features randomise the address.
pub fn is_mac_randomised(mac: &str) -> bool {
    mac.chars().filter(|c| c.is_ascii_hexdigit()).nth(1).map(|c| matches!(c.to_ascii_uppercase(), '2' | '6' | 'A' | 'E')).unwrap_or(false)
}

#[derive(Default)]
struct Guess {
    family: Option<&'static str>,
    version: Option<String>,
    class: Option<&'static str>,
    confidence: u8,
}

fn from_user_agent(ua: &str) -> Guess {
    let l = ua.to_ascii_lowercase();
    // Order matters: iPhone and iPad user agents also say "like Mac OS X".
    if l.contains("iphone") || l.contains("ipod") {
        return Guess { family: Some("iOS"), version: ios_version(ua), class: Some("phone"), confidence: 90 };
    }
    if l.contains("ipad") {
        return Guess { family: Some("iOS"), version: ios_version(ua), class: Some("tablet"), confidence: 90 };
    }
    if l.contains("android") {
        let class = if l.contains("mobile") { "phone" } else { "tablet" };
        return Guess { family: Some("Android"), version: after(ua, "Android "), class: Some(class), confidence: 90 };
    }
    if l.contains("windows nt") {
        let v = after(ua, "Windows NT ");
        // Windows 11 still reports NT 10.0, so a browser cannot tell them apart.
        let version = v.map(|v| if v == "10.0" { "10/11".to_string() } else { v });
        return Guess { family: Some("Windows"), version, class: Some("laptop"), confidence: 85 };
    }
    if l.contains("cros") {
        return Guess { family: Some("ChromeOS"), version: None, class: Some("laptop"), confidence: 85 };
    }
    if l.contains("macintosh") || l.contains("mac os x") {
        return Guess { family: Some("macOS"), version: None, class: Some("laptop"), confidence: 85 };
    }
    if l.contains("smart-tv") || l.contains("smarttv") || l.contains("tizen") || l.contains("webos") {
        return Guess { family: Some("SmartTV"), version: None, class: Some("tv"), confidence: 80 };
    }
    if l.contains("playstation") || l.contains("xbox") || l.contains("nintendo") {
        return Guess { family: Some("Console"), version: None, class: Some("console"), confidence: 80 };
    }
    if l.contains("linux") || l.contains("x11") {
        return Guess { family: Some("Linux"), version: None, class: Some("desktop"), confidence: 70 };
    }
    Guess::default()
}

/// The text after `marker`, up to the next `;` or `)` or space, with `_` read as `.`.
fn after(ua: &str, marker: &str) -> Option<String> {
    let i = ua.find(marker)? + marker.len();
    let v: String = ua[i..].chars().take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '_').collect();
    let v = v.replace('_', ".");
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

fn ios_version(ua: &str) -> Option<String> {
    after(ua, "OS ").or_else(|| after(ua, "Version/"))
}

fn from_dhcp(vendor_class: Option<&str>, hostname: Option<&str>) -> Guess {
    let vc = vendor_class.unwrap_or("").to_ascii_lowercase();
    let hn = hostname.unwrap_or("").to_ascii_lowercase();
    if let Some(rest) = vc.strip_prefix("android-dhcp-") {
        let v: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        return Guess { family: Some("Android"), version: (!v.is_empty()).then_some(v), class: Some("phone"), confidence: 75 };
    }
    if vc.starts_with("msft") {
        return Guess { family: Some("Windows"), version: None, class: Some("laptop"), confidence: 75 };
    }
    if vc.starts_with("dhcpcd") || vc.starts_with("udhcp") {
        return Guess { family: Some("Linux"), version: None, class: Some("desktop"), confidence: 55 };
    }
    if hn.contains("iphone") {
        return Guess { family: Some("iOS"), version: None, class: Some("phone"), confidence: 60 };
    }
    if hn.contains("ipad") {
        return Guess { family: Some("iOS"), version: None, class: Some("tablet"), confidence: 60 };
    }
    if hn.contains("macbook") || hn.contains("imac") || hn.ends_with("-mbp") {
        return Guess { family: Some("macOS"), version: None, class: Some("laptop"), confidence: 60 };
    }
    if hn.starts_with("android-") || hn.contains("galaxy") || hn.contains("pixel") || hn.contains("redmi") || hn.contains("tecno") || hn.contains("infinix") {
        return Guess { family: Some("Android"), version: None, class: Some("phone"), confidence: 55 };
    }
    if hn.starts_with("desktop-") || hn.starts_with("laptop-") {
        return Guess { family: Some("Windows"), version: None, class: Some("laptop"), confidence: 55 };
    }
    Guess::default()
}

/// Combine everything we know.
pub fn identify(c: &Clues<'_>) -> DeviceInfo {
    let ua = c.user_agent.map(from_user_agent).unwrap_or_default();
    let dh = from_dhcp(c.dhcp_vendor_class, c.dhcp_hostname);
    let randomised = c.mac.map(is_mac_randomised).unwrap_or(false);

    // Prefer the browser; fall back to DHCP.
    let (best, other) = if ua.confidence >= dh.confidence { (ua, dh) } else { (dh, ua) };
    let mut confidence = best.confidence;
    // Two independent sources that agree make us more sure; a clash makes us less.
    if best.family.is_some() && other.family.is_some() {
        if best.family == other.family {
            confidence = (confidence + 8).min(98);
        } else {
            confidence = confidence.saturating_sub(25);
        }
    }
    if best.family.is_none() {
        confidence = if randomised { 5 } else { 10 };
    }
    DeviceInfo {
        os_family: best.family.map(str::to_string),
        os_version: best.version.or(other.version),
        device_class: best.class.or(other.class).unwrap_or("unknown").to_string(),
        mac_randomised: randomised,
        confidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANDROID_UA: &str = "Mozilla/5.0 (Linux; Android 14; SM-S911B) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Mobile Safari/537.36";
    const IPHONE_UA: &str =
        "Mozilla/5.0 (iPhone; CPU iPhone OS 17_2 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.2 Mobile/15E148 Safari/604.1";
    const WIN_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36";
    const MAC_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.1 Safari/605.1.15";

    fn ua(s: &str) -> DeviceInfo {
        identify(&Clues { user_agent: Some(s), ..Default::default() })
    }

    #[test]
    fn reads_android_phone() {
        let d = ua(ANDROID_UA);
        assert_eq!((d.os_family.as_deref(), d.os_version.as_deref(), d.device_class.as_str()), (Some("Android"), Some("14"), "phone"));
    }

    #[test]
    fn iphone_is_not_mistaken_for_a_mac() {
        let d = ua(IPHONE_UA);
        assert_eq!((d.os_family.as_deref(), d.os_version.as_deref(), d.device_class.as_str()), (Some("iOS"), Some("17.2"), "phone"));
    }

    #[test]
    fn reads_windows_mac_and_tablets() {
        assert_eq!(ua(WIN_UA).os_family.as_deref(), Some("Windows"));
        assert_eq!(ua(WIN_UA).os_version.as_deref(), Some("10/11"), "a browser cannot tell 10 from 11");
        assert_eq!(ua(MAC_UA).os_family.as_deref(), Some("macOS"));
        assert_eq!(ua("Mozilla/5.0 (Linux; Android 13; SM-X700) Chrome/120.0 Safari/537.36").device_class, "tablet");
        assert_eq!(ua("Mozilla/5.0 (iPad; CPU OS 16_0 like Mac OS X) Version/16.0 Safari/604.1").device_class, "tablet");
    }

    #[test]
    fn dhcp_alone_still_gives_an_answer_with_lower_confidence() {
        let d = identify(&Clues { dhcp_vendor_class: Some("android-dhcp-13"), ..Default::default() });
        assert_eq!((d.os_family.as_deref(), d.os_version.as_deref()), (Some("Android"), Some("13")));
        assert!(d.confidence < 90 && d.confidence >= 70);
        let w = identify(&Clues { dhcp_vendor_class: Some("MSFT 5.0"), ..Default::default() });
        assert_eq!(w.os_family.as_deref(), Some("Windows"));
        let h = identify(&Clues { dhcp_hostname: Some("Amina-iPhone"), ..Default::default() });
        assert_eq!(h.os_family.as_deref(), Some("iOS"));
    }

    #[test]
    fn agreeing_sources_raise_confidence_and_clashing_ones_lower_it() {
        let agree = identify(&Clues { user_agent: Some(ANDROID_UA), dhcp_vendor_class: Some("android-dhcp-14"), ..Default::default() });
        let alone = ua(ANDROID_UA);
        assert!(agree.confidence > alone.confidence);
        let clash = identify(&Clues { user_agent: Some(IPHONE_UA), dhcp_vendor_class: Some("MSFT 5.0"), ..Default::default() });
        assert!(clash.confidence < alone.confidence, "an iPhone that speaks Windows DHCP is suspicious");
    }

    #[test]
    fn unknown_devices_say_so() {
        let d = identify(&Clues::default());
        assert_eq!((d.device_class.as_str(), d.os_family), ("unknown", None));
        assert!(d.confidence <= 10);
    }

    #[test]
    fn random_macs_are_recognised() {
        assert!(is_mac_randomised("DA:A1:19:00:00:01"));
        assert!(is_mac_randomised("02:00:00:00:00:01"));
        assert!(is_mac_randomised("fe-aa-bb-cc-dd-ee"));
        assert!(!is_mac_randomised("A4:83:E7:12:34:56"));
        assert!(!is_mac_randomised("3C:22:FB:00:00:01"));
        assert!(!is_mac_randomised(""));
        assert!(identify(&Clues { mac: Some("DA:A1:19:00:00:01"), ..Default::default() }).mac_randomised);
    }
}
