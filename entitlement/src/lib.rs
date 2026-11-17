//! # Entitlement engine
//!
//! The rules that decide what a paying customer may do on the network. It is a
//! *pure* library: no database, no network, no clock. Callers pass the current
//! time in, so every rule can be tested exactly.
//!
//! ```text
//!   Plan ──grant──▶ Entitlement ──select──▶ session_limits ──▶ RADIUS reply
//!                        ▲                                         │
//!                        └──── apply_usage ◀── accounting updates ◀┘
//! ```
//!
//! * [`plan`]        what is sold (time, data or hybrid, devices, speed, fair use)
//! * [`entitlement`] what a customer owns, and which one is used first
//! * [`meter`]       turns 60-second accounting updates into charges and decisions
//! * [`slots`]       device slots for single and multi-device plans
//! * [`radius`]      formats the limits as FreeRADIUS / MikroTik reply attributes
//!
//! Time is carried as Unix seconds ([`UnixSecs`]) and byte counts as `u64`.

pub mod entitlement;
pub mod meter;
pub mod plan;
pub mod radius;
pub mod slots;

pub use entitlement::{select_index, Entitlement, LimitError, SessionLimits, Status};
pub use meter::{apply_usage, Action, DisconnectReason, Outcome, SessionState, UsageSample};
pub use plan::{Burst, FairUse, Plan, PlanError, PlanKind, SpeedProfile};
pub use slots::{BindError, BindOutcome, DeviceSlots, SwapError};

/// Seconds since 1970-01-01 UTC.
pub type UnixSecs = i64;

/// Seconds in one day.
pub const DAY_SECS: i64 = 86_400;

/// How often the router sends an accounting update, in seconds. Active-time
/// counting is built around this interval.
pub const INTERIM_SECS: u32 = 60;

/// The local calendar day number for `now`, given the site's offset from UTC.
/// Nigeria is UTC+1, so `tz_offset_secs` is `3600`. Used to reset the
/// fair-use counter at local midnight.
pub fn local_day(now: UnixSecs, tz_offset_secs: i32) -> i64 {
    (now + i64::from(tz_offset_secs)).div_euclid(DAY_SECS)
}
