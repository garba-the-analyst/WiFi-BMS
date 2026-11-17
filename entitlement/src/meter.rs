//! The meter: turns one accounting update into charges and a decision.
//!
//! The router reports usage every [`crate::INTERIM_SECS`] seconds. For each
//! report the meter decides:
//!
//! 1. **Was this interval active?** Yes when the device moved at least
//!    `plan.active_threshold_bytes`. Only active intervals use up the
//!    active-time budget, so a customer who leaves a phone idle in a pocket is
//!    not charged for the idle time.
//! 2. **How many bytes to charge** against a data budget (all traffic counts).
//! 3. **Should the session go on?** Continue, or disconnect with a reason.
//! 4. **Is fair use now in force?** Reported as `throttled`; the caller changes
//!    the router's rate limit when it flips.

use crate::entitlement::Entitlement;
use crate::{local_day, UnixSecs};

/// Usage reported since the previous update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageSample {
    pub bytes_in: u64,
    pub bytes_out: u64,
    /// Seconds this sample covers (normally 60).
    pub interval_secs: u32,
}

/// Per-session counters kept by the site agent between updates.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionState {
    /// Seconds in a row without an active interval.
    pub idle_secs: u32,
    /// Active seconds charged in this session.
    pub active_secs: u32,
    /// Bytes charged in this session.
    pub bytes: u64,
}

/// Why a session is ended by the platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisconnectReason {
    Idle,
    TimeExhausted,
    DataExhausted,
    Expired,
}

/// What to do after this update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Continue,
    Disconnect(DisconnectReason),
}

/// The result of one meter step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    pub action: Action,
    /// Whether the interval counted as active.
    pub active: bool,
    /// Active seconds taken from the time budget in this step.
    pub charged_active_secs: u32,
    /// Bytes taken from the data budget in this step.
    pub charged_bytes: u64,
    /// Whether the fair-use speed applies after this step.
    pub throttled: bool,
}

/// Apply one accounting update to an entitlement.
///
/// Mutates `ent` (remaining budgets, fair-use counter, status) and `session`
/// (idle run, totals). The caller writes the charges to the usage ledger.
pub fn apply_usage(
    ent: &mut Entitlement,
    session: &mut SessionState,
    sample: UsageSample,
    now: UnixSecs,
    tz_offset_secs: i32,
) -> Outcome {
    let total = sample.bytes_in.saturating_add(sample.bytes_out);
    let active = total >= ent.plan.active_threshold_bytes;

    // Time budget: only active intervals are charged.
    let mut charged_active_secs = 0u32;
    if let Some(rem) = ent.remaining_active_secs {
        if active {
            charged_active_secs = sample.interval_secs.min(rem);
            ent.remaining_active_secs = Some(rem - charged_active_secs);
        }
    }

    // Data budget: every byte counts, capped at what is left.
    let mut charged_bytes = 0u64;
    if let Some(rem) = ent.remaining_bytes {
        charged_bytes = total.min(rem);
        ent.remaining_bytes = Some(rem - charged_bytes);
    }

    // Fair use: count today's bytes, resetting at local midnight.
    if ent.plan.fair_use.is_some() {
        let today = local_day(now, tz_offset_secs);
        if ent.fup_day != Some(today) {
            ent.fup_day = Some(today);
            ent.fup_bytes_today = 0;
        }
        ent.fup_bytes_today = ent.fup_bytes_today.saturating_add(total);
    }

    // Idle tracking.
    if active {
        session.idle_secs = 0;
    } else {
        session.idle_secs = session.idle_secs.saturating_add(sample.interval_secs);
    }
    session.active_secs = session.active_secs.saturating_add(charged_active_secs);
    session.bytes = session.bytes.saturating_add(charged_bytes);

    // Decision, in order of severity.
    let action = if now >= ent.expires_at {
        Action::Disconnect(DisconnectReason::Expired)
    } else if ent.remaining_active_secs == Some(0) {
        Action::Disconnect(DisconnectReason::TimeExhausted)
    } else if ent.remaining_bytes == Some(0) {
        Action::Disconnect(DisconnectReason::DataExhausted)
    } else if session.idle_secs >= ent.plan.idle_timeout_secs {
        Action::Disconnect(DisconnectReason::Idle)
    } else {
        Action::Continue
    };

    // A budget or window that has run out ends the purchase. Idle only ends the session.
    if matches!(
        action,
        Action::Disconnect(DisconnectReason::Expired | DisconnectReason::TimeExhausted | DisconnectReason::DataExhausted)
    ) {
        ent.refresh_status(now);
    }

    Outcome {
        action,
        active,
        charged_active_secs,
        charged_bytes,
        throttled: ent.fair_use_throttled(now, tz_offset_secs),
    }
}
