//! What a customer owns: an [`Entitlement`], and which one is used first.

use std::fmt;

use crate::plan::{Plan, PlanError, SpeedProfile};
use crate::{local_day, UnixSecs, INTERIM_SECS};

/// Lifecycle of a purchase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum Status {
    /// Bought but not yet used (plans that start on first use).
    Pending,
    Active,
    /// A budget ran out.
    Exhausted,
    /// The validity window closed.
    Expired,
    Cancelled,
}

/// One purchase, with the plan frozen as it was sold.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Entitlement {
    pub id: String,
    pub plan: Plan,
    pub status: Status,
    pub granted_at: UnixSecs,
    pub activated_at: Option<UnixSecs>,
    /// Always set. Before activation it is the "unused" expiry; after
    /// activation it is `activated_at + valid_seconds`.
    pub expires_at: UnixSecs,
    /// Active seconds left. `None` when the plan has no time budget.
    pub remaining_active_secs: Option<u32>,
    /// Bytes left. `None` when the plan has no data budget.
    pub remaining_bytes: Option<u64>,
    /// Local day number that `fup_bytes_today` belongs to.
    pub fup_day: Option<i64>,
    pub fup_bytes_today: u64,
}

impl Entitlement {
    /// Create an entitlement from a validated plan at purchase time.
    pub fn grant(id: impl Into<String>, plan: Plan, now: UnixSecs) -> Result<Self, PlanError> {
        plan.validate()?;
        let (status, activated_at, expires_at) = if plan.activate_on_first_use {
            (Status::Pending, None, now + i64::from(plan.unused_expiry_seconds))
        } else {
            (Status::Active, Some(now), now + i64::from(plan.valid_seconds))
        };
        Ok(Entitlement {
            id: id.into(),
            remaining_active_secs: plan.active_seconds,
            remaining_bytes: plan.data_bytes,
            plan,
            status,
            granted_at: now,
            activated_at,
            expires_at,
            fup_day: None,
            fup_bytes_today: 0,
        })
    }

    /// Start the validity window if the plan starts on first use. Returns
    /// `true` when this call activated it.
    pub fn activate(&mut self, now: UnixSecs) -> bool {
        if self.status == Status::Pending && now < self.expires_at {
            self.status = Status::Active;
            self.activated_at = Some(now);
            self.expires_at = now + i64::from(self.plan.valid_seconds);
            true
        } else {
            false
        }
    }

    fn has_budget(&self) -> bool {
        self.remaining_active_secs.is_none_or(|s| s > 0) && self.remaining_bytes.is_none_or(|b| b > 0)
    }

    /// May this purchase be used right now?
    pub fn is_usable(&self, now: UnixSecs) -> bool {
        matches!(self.status, Status::Pending | Status::Active) && now < self.expires_at && self.has_budget()
    }

    /// Bring `status` up to date: expired if the window closed, exhausted if a
    /// budget is empty. Cancelled and already-terminal states are left alone.
    pub fn refresh_status(&mut self, now: UnixSecs) {
        if matches!(self.status, Status::Cancelled | Status::Exhausted | Status::Expired) {
            return;
        }
        if now >= self.expires_at {
            self.status = Status::Expired;
        } else if !self.has_budget() {
            self.status = Status::Exhausted;
        }
    }

    /// True when today's fair-use cap has been reached.
    pub fn fair_use_throttled(&self, now: UnixSecs, tz_offset_secs: i32) -> bool {
        match &self.plan.fair_use {
            Some(fu) => self.fup_day == Some(local_day(now, tz_offset_secs)) && self.fup_bytes_today >= fu.daily_bytes,
            None => false,
        }
    }

    /// The limits to hand to the router for a new or refreshed session.
    ///
    /// Call this again whenever you want to refresh the router, for example
    /// after idle gaps: `session_timeout_secs` shrinks to the remaining active
    /// time, and the site agent pushes the new value with a RADIUS CoA.
    pub fn session_limits(&self, now: UnixSecs, tz_offset_secs: i32) -> Result<SessionLimits, LimitError> {
        if !self.is_usable(now) {
            return Err(LimitError::NotUsable(if now >= self.expires_at {
                Status::Expired
            } else if !self.has_budget() {
                Status::Exhausted
            } else {
                self.status
            }));
        }
        let until_expiry = (self.expires_at - now).clamp(1, i64::from(u32::MAX)) as u32;
        let session_timeout_secs = match self.remaining_active_secs {
            Some(rem) => rem.min(until_expiry),
            None => until_expiry,
        };
        let throttled = self.fair_use_throttled(now, tz_offset_secs);
        let speed = match (&self.plan.fair_use, throttled) {
            (Some(fu), true) => fu.throttled.clone(),
            _ => self.plan.speed.clone(),
        };
        Ok(SessionLimits {
            session_timeout_secs,
            idle_timeout_secs: self.plan.idle_timeout_secs,
            interim_secs: INTERIM_SECS,
            speed,
            total_limit_bytes: self.remaining_bytes,
            throttled,
        })
    }
}

/// The limits for one session, ready to be turned into RADIUS attributes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLimits {
    /// Router ends the session after this many seconds.
    pub session_timeout_secs: u32,
    /// Router ends the session after this many seconds without traffic.
    pub idle_timeout_secs: u32,
    /// Accounting update interval.
    pub interim_secs: u32,
    pub speed: SpeedProfile,
    /// Bytes the router may still pass in total, if the plan has a data budget.
    pub total_limit_bytes: Option<u64>,
    /// Whether the fair-use speed is currently in force.
    pub throttled: bool,
}

/// Why no session limits could be produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitError {
    NotUsable(Status),
}

impl fmt::Display for LimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LimitError::NotUsable(s) => write!(f, "entitlement cannot be used (status {s:?})"),
        }
    }
}

impl std::error::Error for LimitError {}

/// Choose which of a customer's purchases to use now: of the usable ones, the
/// one that **expires soonest** (ties broken by oldest purchase, then id), so
/// nothing is wasted. Returns its index.
pub fn select_index(entitlements: &[Entitlement], now: UnixSecs) -> Option<usize> {
    entitlements
        .iter()
        .enumerate()
        .filter(|(_, e)| e.is_usable(now))
        .min_by(|(_, a), (_, b)| (a.expires_at, a.granted_at, &a.id).cmp(&(b.expires_at, b.granted_at, &b.id)))
        .map(|(i, _)| i)
}
