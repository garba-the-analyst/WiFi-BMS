//! End-to-end rule tests for the entitlement engine, written as customer
//! stories. Each test name says the rule it protects.

use ::entitlement::radius::reply_attributes;
use ::entitlement::*;

const TZ: i32 = 3600; // Nigeria, UTC+1
const T0: UnixSecs = 1_800_000_000; // an arbitrary start time
const MINUTE: u32 = 60;
const ACTIVE: u64 = 100_000; // bytes per minute that count as active (threshold is 20 KB)

fn speed(down: u32, up: u32) -> SpeedProfile {
    SpeedProfile { down_kbps: down, up_kbps: up, burst: None, priority: 8 }
}

fn base_plan(kind: PlanKind) -> Plan {
    Plan {
        kind,
        active_seconds: None,
        data_bytes: None,
        valid_seconds: 86_400,
        activate_on_first_use: true,
        unused_expiry_seconds: 90 * 86_400,
        device_limit: 1,
        swap_cooldown_secs: 86_400,
        speed: speed(5000, 2000),
        active_threshold_bytes: 20_480,
        idle_timeout_secs: 300,
        fair_use: None,
    }
}

fn time_plan(active_secs: u32) -> Plan {
    Plan { active_seconds: Some(active_secs), ..base_plan(PlanKind::Time) }
}

fn data_plan(bytes: u64) -> Plan {
    Plan { data_bytes: Some(bytes), ..base_plan(PlanKind::Data) }
}

fn sample(bytes: u64) -> UsageSample {
    UsageSample { bytes_in: bytes / 2, bytes_out: bytes - bytes / 2, interval_secs: MINUTE }
}

// ---------------------------------------------------------------- plan rules

#[test]
fn plan_budgets_must_match_the_kind() {
    assert_eq!(base_plan(PlanKind::Time).validate(), Err(PlanError::MissingTimeBudget));
    assert_eq!(base_plan(PlanKind::Data).validate(), Err(PlanError::MissingDataBudget));
    let both = Plan { active_seconds: Some(60), data_bytes: Some(1), ..base_plan(PlanKind::Time) };
    assert_eq!(both.validate(), Err(PlanError::UnexpectedDataBudget));
    let both = Plan { active_seconds: Some(60), data_bytes: Some(1), ..base_plan(PlanKind::Data) };
    assert_eq!(both.validate(), Err(PlanError::UnexpectedTimeBudget));
    let hybrid = Plan { active_seconds: Some(60), data_bytes: Some(1), ..base_plan(PlanKind::Hybrid) };
    assert_eq!(hybrid.validate(), Ok(()));
    let zero = Plan { active_seconds: Some(0), ..base_plan(PlanKind::Time) };
    assert_eq!(zero.validate(), Err(PlanError::ZeroBudget));
}

#[test]
fn granting_rejects_an_invalid_plan() {
    assert!(Entitlement::grant("e1", base_plan(PlanKind::Time), T0).is_err());
}

// ------------------------------------------------------- activation and expiry

#[test]
fn validity_starts_on_first_use_when_the_plan_says_so() {
    let mut e = Entitlement::grant("e1", time_plan(3600), T0).unwrap();
    assert_eq!(e.status, Status::Pending);
    assert_eq!(e.expires_at, T0 + 90 * 86_400, "unused purchases expire after the unused window");

    let ten_days = T0 + 10 * 86_400;
    assert!(e.activate(ten_days));
    assert_eq!(e.status, Status::Active);
    assert_eq!(e.expires_at, ten_days + 86_400, "window now runs from first use");
    assert!(!e.activate(ten_days + 5), "a second activation changes nothing");
}

#[test]
fn validity_starts_at_purchase_when_not_on_first_use() {
    let plan = Plan { activate_on_first_use: false, ..time_plan(3600) };
    let e = Entitlement::grant("e1", plan, T0).unwrap();
    assert_eq!(e.status, Status::Active);
    assert_eq!(e.expires_at, T0 + 86_400);
}

#[test]
fn an_unused_purchase_expires() {
    let mut e = Entitlement::grant("e1", time_plan(3600), T0).unwrap();
    let late = T0 + 91 * 86_400;
    assert!(!e.is_usable(late));
    assert!(!e.activate(late));
    e.refresh_status(late);
    assert_eq!(e.status, Status::Expired);
}

// ------------------------------------------------------------ active-time meter

#[test]
fn only_active_minutes_use_up_a_time_plan() {
    let mut e = Entitlement::grant("e1", time_plan(3600), T0).unwrap();
    e.activate(T0);
    let mut s = SessionState::default();

    // Pattern: 5 active minutes, then 3 idle minutes (under the 5-minute idle timeout), repeated.
    let mut active_minutes = 0u32;
    for i in 0..60u32 {
        let is_active = i % 8 < 5;
        let bytes = if is_active { ACTIVE } else { 0 };
        let o = apply_usage(&mut e, &mut s, sample(bytes), T0 + i64::from(i + 1) * 60, TZ);
        assert_eq!(o.action, Action::Continue, "minute {i}");
        assert_eq!(o.active, is_active);
        if is_active {
            active_minutes += 1;
        }
    }
    assert_eq!(e.remaining_active_secs, Some(3600 - active_minutes * 60));
    assert_eq!(s.active_secs, active_minutes * 60);
}

#[test]
fn an_idle_device_is_disconnected_after_the_idle_timeout_and_the_clock_stops() {
    let mut e = Entitlement::grant("e1", time_plan(3600), T0).unwrap();
    e.activate(T0);
    let mut s = SessionState::default();
    for i in 1..=4 {
        let o = apply_usage(&mut e, &mut s, sample(0), T0 + i * 60, TZ);
        assert_eq!(o.action, Action::Continue, "idle minute {i}");
    }
    let o = apply_usage(&mut e, &mut s, sample(0), T0 + 5 * 60, TZ);
    assert_eq!(o.action, Action::Disconnect(DisconnectReason::Idle));
    assert_eq!(e.remaining_active_secs, Some(3600), "idle time costs nothing");
    assert_eq!(e.status, Status::Active, "idle ends the session, not the purchase");
}

#[test]
fn activity_resets_the_idle_run() {
    let mut e = Entitlement::grant("e1", time_plan(3600), T0).unwrap();
    e.activate(T0);
    let mut s = SessionState::default();
    for i in 1..=4 {
        apply_usage(&mut e, &mut s, sample(0), T0 + i * 60, TZ);
    }
    assert_eq!(s.idle_secs, 240);
    apply_usage(&mut e, &mut s, sample(ACTIVE), T0 + 5 * 60, TZ);
    assert_eq!(s.idle_secs, 0);
}

#[test]
fn a_time_plan_ends_when_active_time_is_used_up() {
    let mut e = Entitlement::grant("e1", time_plan(120), T0).unwrap();
    e.activate(T0);
    let mut s = SessionState::default();
    assert_eq!(apply_usage(&mut e, &mut s, sample(ACTIVE), T0 + 60, TZ).action, Action::Continue);
    let o = apply_usage(&mut e, &mut s, sample(ACTIVE), T0 + 120, TZ);
    assert_eq!(o.action, Action::Disconnect(DisconnectReason::TimeExhausted));
    assert_eq!(e.remaining_active_secs, Some(0));
    assert_eq!(e.status, Status::Exhausted);
    assert!(!e.is_usable(T0 + 121));
}

#[test]
fn the_last_partial_interval_is_capped_at_what_is_left() {
    let mut e = Entitlement::grant("e1", time_plan(90), T0).unwrap();
    e.activate(T0);
    let mut s = SessionState::default();
    apply_usage(&mut e, &mut s, sample(ACTIVE), T0 + 60, TZ);
    let o = apply_usage(&mut e, &mut s, sample(ACTIVE), T0 + 120, TZ);
    assert_eq!(o.charged_active_secs, 30);
    assert_eq!(e.remaining_active_secs, Some(0));
}

// ------------------------------------------------------------------ data plans

#[test]
fn a_data_plan_counts_every_byte_and_ignores_time() {
    let mut e = Entitlement::grant("e1", data_plan(1000), T0).unwrap();
    e.activate(T0);
    let mut s = SessionState::default();

    // 600 bytes is below the active threshold: data still counts, no time is charged.
    let o = apply_usage(&mut e, &mut s, sample(600), T0 + 60, TZ);
    assert_eq!((o.charged_bytes, o.charged_active_secs), (600, 0));
    assert_eq!(e.remaining_bytes, Some(400));
    assert_eq!(e.remaining_active_secs, None);

    let o = apply_usage(&mut e, &mut s, sample(600), T0 + 120, TZ);
    assert_eq!(o.charged_bytes, 400, "charge is capped at what is left");
    assert_eq!(o.action, Action::Disconnect(DisconnectReason::DataExhausted));
    assert_eq!(e.status, Status::Exhausted);
}

#[test]
fn a_hybrid_plan_ends_when_the_first_budget_runs_out() {
    // threshold lowered so the 1000-byte sample counts as active use
    let plan = Plan { active_seconds: Some(3600), data_bytes: Some(1000), active_threshold_bytes: 100, ..base_plan(PlanKind::Hybrid) };
    let mut e = Entitlement::grant("e1", plan, T0).unwrap();
    e.activate(T0);
    let mut s = SessionState::default();
    let o = apply_usage(&mut e, &mut s, sample(1000), T0 + 60, TZ);
    assert_eq!(o.action, Action::Disconnect(DisconnectReason::DataExhausted));
    assert_eq!(e.remaining_active_secs, Some(3600 - 60), "time was used too, but data ran out first");
}

#[test]
fn the_validity_window_ends_a_plan_even_with_budget_left() {
    let mut e = Entitlement::grant("e1", time_plan(3600), T0).unwrap();
    e.activate(T0);
    let mut s = SessionState::default();
    let o = apply_usage(&mut e, &mut s, sample(ACTIVE), T0 + 86_400, TZ);
    assert_eq!(o.action, Action::Disconnect(DisconnectReason::Expired));
    assert_eq!(e.status, Status::Expired);
}

// ------------------------------------------------------------------- fair use

fn fair_use_plan() -> Plan {
    Plan { fair_use: Some(FairUse { daily_bytes: 1000, throttled: speed(1000, 500) }), ..data_plan(10_000_000) }
}

#[test]
fn fair_use_slows_the_customer_after_the_daily_cap_and_resets_at_local_midnight() {
    let mut e = Entitlement::grant("e1", fair_use_plan(), T0).unwrap();
    e.activate(T0);
    let mut s = SessionState::default();

    assert!(!apply_usage(&mut e, &mut s, sample(600), T0 + 60, TZ).throttled);
    assert!(apply_usage(&mut e, &mut s, sample(600), T0 + 120, TZ).throttled, "1200 bytes passes the 1000-byte cap");
    let limits = e.session_limits(T0 + 130, TZ).unwrap();
    assert!(limits.throttled);
    assert_eq!(limits.speed, speed(1000, 500));

    // The next local day starts fresh.
    let o = apply_usage(&mut e, &mut s, sample(100), T0 + 86_400 + 60, TZ);
    assert!(!o.throttled);
    assert_eq!(e.fup_bytes_today, 100);
}

#[test]
fn local_day_follows_the_site_timezone() {
    // 23:30 UTC is already the next day at UTC+1.
    let midnight_utc = 1_800_000_000 - (1_800_000_000 % 86_400);
    let before = midnight_utc + 23 * 3600 + 30 * 60;
    assert_eq!(local_day(before, 0) + 1, local_day(before, 3600));
}

// ------------------------------------------------------------ choosing a purchase

#[test]
fn the_purchase_that_expires_soonest_is_used_first() {
    let mut a = Entitlement::grant("a", time_plan(3600), T0).unwrap();
    let mut b = Entitlement::grant("b", time_plan(3600), T0).unwrap();
    let mut c = Entitlement::grant("c", time_plan(3600), T0).unwrap();
    a.expires_at = T0 + 100;
    b.expires_at = T0 + 50;
    c.expires_at = T0 + 10;
    c.remaining_active_secs = Some(0); // exhausted: never chosen
    let all = [a, b, c];

    assert_eq!(select_index(&all, T0), Some(1), "b expires before a, and c is used up");
    assert_eq!(select_index(&all, T0 + 60), Some(0), "b has expired, so a");
    assert_eq!(select_index(&all, T0 + 200), None, "everything has expired");
}

// ------------------------------------------------------------------ RADIUS reply

#[test]
fn session_limits_use_the_smaller_of_remaining_time_and_expiry() {
    let mut e = Entitlement::grant("e1", time_plan(3600), T0).unwrap();
    e.activate(T0);
    let l = e.session_limits(T0, TZ).unwrap();
    assert_eq!(l.session_timeout_secs, 3600, "remaining active time is smaller than the 24 h window");
    assert_eq!(l.total_limit_bytes, None);

    e.remaining_active_secs = Some(100_000);
    let l = e.session_limits(T0, TZ).unwrap();
    assert_eq!(l.session_timeout_secs, 86_400, "now the window is smaller");
}

#[test]
fn reply_attributes_for_a_time_plan() {
    let mut e = Entitlement::grant("e1", time_plan(3600), T0).unwrap();
    e.activate(T0);
    let attrs = reply_attributes(&e.session_limits(T0, TZ).unwrap());
    let get = |k: &str| attrs.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    assert_eq!(get("Session-Timeout"), Some("3600"));
    assert_eq!(get("Idle-Timeout"), Some("300"));
    assert_eq!(get("Acct-Interim-Interval"), Some("60"));
    assert_eq!(get("Mikrotik-Rate-Limit"), Some("2000k/5000k"));
    assert_eq!(get("Mikrotik-Total-Limit"), None);
}

#[test]
fn reply_attributes_for_a_big_data_plan_use_gigawords() {
    let mut e = Entitlement::grant("e1", data_plan(5 * 1024 * 1024 * 1024), T0).unwrap();
    e.activate(T0);
    let attrs = reply_attributes(&e.session_limits(T0, TZ).unwrap());
    let get = |k: &str| attrs.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    assert_eq!(get("Mikrotik-Total-Limit"), Some("1073741824"));
    assert_eq!(get("Mikrotik-Total-Limit-Gigawords"), Some("1"));
}

#[test]
fn no_limits_for_an_entitlement_that_cannot_be_used() {
    let mut e = Entitlement::grant("e1", time_plan(60), T0).unwrap();
    e.activate(T0);
    e.remaining_active_secs = Some(0);
    assert_eq!(e.session_limits(T0 + 1, TZ), Err(LimitError::NotUsable(Status::Exhausted)));
    let mut e = Entitlement::grant("e2", time_plan(60), T0).unwrap();
    e.activate(T0);
    assert_eq!(e.session_limits(T0 + 90_000, TZ), Err(LimitError::NotUsable(Status::Expired)));
}

// ----------------------------------------------------------------- device slots

#[test]
fn a_single_device_plan_allows_one_device() {
    let mut slots = DeviceSlots::new(1, 86_400);
    assert_eq!(slots.bind("phone", T0), Ok(BindOutcome::Bound));
    assert_eq!(slots.bind("phone", T0 + 5), Ok(BindOutcome::AlreadyBound));
    assert_eq!(slots.bind("laptop", T0 + 10), Err(BindError::SlotsFull { can_swap_now: true, next_swap_at: T0 + 10 }));
}

#[test]
fn swaps_are_limited_to_one_per_cooldown() {
    let mut slots = DeviceSlots::new(2, 86_400);
    slots.bind("a", T0).unwrap();
    slots.bind("b", T0).unwrap();
    assert_eq!(slots.swap("a", "c", T0 + 100), Ok(()));
    assert!(slots.bind("a", T0 + 200).is_err(), "a lost its slot");
    assert_eq!(slots.swap("b", "d", T0 + 200), Err(SwapError::Cooldown { next_swap_at: T0 + 100 + 86_400 }));
    assert_eq!(slots.bind("e", T0 + 300), Err(BindError::SlotsFull { can_swap_now: false, next_swap_at: T0 + 100 + 86_400 }));
    assert_eq!(slots.swap("b", "d", T0 + 100 + 86_400), Ok(()));
}

#[test]
fn swap_errors() {
    let mut slots = DeviceSlots::new(2, 60);
    slots.bind("a", T0).unwrap();
    slots.bind("b", T0).unwrap();
    assert_eq!(slots.swap("x", "c", T0), Err(SwapError::NotBound));
    assert_eq!(slots.swap("a", "b", T0), Err(SwapError::AlreadyBound));
    assert!(slots.release("a"));
    assert_eq!(slots.bind("c", T0), Ok(BindOutcome::Bound));
}

// ---------------------------------------------------------------- serialisation

#[cfg(feature = "serde")]
#[test]
fn an_entitlement_round_trips_through_json_for_the_plan_snapshot_column() {
    let mut e = Entitlement::grant("e1", fair_use_plan(), T0).unwrap();
    e.activate(T0);
    let json = serde_json::to_string(&e).unwrap();
    let back: Entitlement = serde_json::from_str(&json).unwrap();
    assert_eq!(e, back);
}
