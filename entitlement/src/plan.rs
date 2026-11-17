//! What is sold: a [`Plan`] and the [`SpeedProfile`] attached to it.

use std::fmt;

/// The three plan families.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum PlanKind {
    /// Counts **active** seconds only (hourly, daily, weekly, monthly plans).
    Time,
    /// Counts bytes, whether or not the device is otherwise active.
    Data,
    /// Both budgets apply; the first to run out ends access.
    Hybrid,
}

/// Optional burst allowance: the customer may exceed the base rate briefly.
/// All rates are kilobits per second.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Burst {
    pub down_kbps: u32,
    pub up_kbps: u32,
    /// Average rate above which the burst stops being granted.
    pub threshold_kbps: u32,
    /// Window, in seconds, over which the average is measured.
    pub time_secs: u32,
}

/// Download and upload rate. It is kept separate from the plan so a speed
/// change never means editing every plan.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SpeedProfile {
    pub down_kbps: u32,
    pub up_kbps: u32,
    pub burst: Option<Burst>,
    /// Queue priority, 1 (highest) to 8 (lowest).
    pub priority: u8,
}

/// Fair-use policy: after `daily_bytes` in one local day, the customer is moved
/// to the slower `throttled` profile until local midnight.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FairUse {
    pub daily_bytes: u64,
    pub throttled: SpeedProfile,
}

/// A product the operator sells. The budgets, validity and limits here are
/// copied into every [`crate::Entitlement`] at purchase time, so editing a
/// plan later never changes what a customer already bought.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Plan {
    pub kind: PlanKind,
    /// Active-time budget in seconds. Required for `Time` and `Hybrid`.
    pub active_seconds: Option<u32>,
    /// Data budget in bytes. Required for `Data` and `Hybrid`.
    pub data_bytes: Option<u64>,
    /// How long the purchase stays valid once it starts, in seconds.
    pub valid_seconds: u32,
    /// `true`: the validity window starts at the first connection.
    /// `false`: it starts at purchase.
    pub activate_on_first_use: bool,
    /// A purchase that is never used expires this many seconds after purchase.
    pub unused_expiry_seconds: u32,
    /// How many devices may use the plan at once.
    pub device_limit: u8,
    /// Minimum seconds between device swaps once all slots are full.
    pub swap_cooldown_secs: u32,
    pub speed: SpeedProfile,
    /// Bytes moved in one accounting interval for the minute to count as active.
    pub active_threshold_bytes: u64,
    /// A device with no active minutes for this long is disconnected.
    pub idle_timeout_secs: u32,
    pub fair_use: Option<FairUse>,
}

/// Why a plan definition is not valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanError {
    MissingTimeBudget,
    MissingDataBudget,
    UnexpectedTimeBudget,
    UnexpectedDataBudget,
    ZeroBudget,
    ZeroValidity,
    ZeroDeviceLimit,
    ZeroFairUseCap,
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            PlanError::MissingTimeBudget => "a time or hybrid plan needs an active-time budget",
            PlanError::MissingDataBudget => "a data or hybrid plan needs a data budget",
            PlanError::UnexpectedTimeBudget => "a data plan must not carry an active-time budget",
            PlanError::UnexpectedDataBudget => "a time plan must not carry a data budget",
            PlanError::ZeroBudget => "a budget must be greater than zero",
            PlanError::ZeroValidity => "validity must be greater than zero",
            PlanError::ZeroDeviceLimit => "a plan must allow at least one device",
            PlanError::ZeroFairUseCap => "the fair-use cap must be greater than zero",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for PlanError {}

impl Plan {
    /// Check the plan is internally consistent. The same rules are enforced in
    /// the database by the `plan_budget_matches_kind` constraint.
    pub fn validate(&self) -> Result<(), PlanError> {
        match self.kind {
            PlanKind::Time => {
                if self.active_seconds.is_none() {
                    return Err(PlanError::MissingTimeBudget);
                }
                if self.data_bytes.is_some() {
                    return Err(PlanError::UnexpectedDataBudget);
                }
            }
            PlanKind::Data => {
                if self.data_bytes.is_none() {
                    return Err(PlanError::MissingDataBudget);
                }
                if self.active_seconds.is_some() {
                    return Err(PlanError::UnexpectedTimeBudget);
                }
            }
            PlanKind::Hybrid => {
                if self.active_seconds.is_none() {
                    return Err(PlanError::MissingTimeBudget);
                }
                if self.data_bytes.is_none() {
                    return Err(PlanError::MissingDataBudget);
                }
            }
        }
        if self.active_seconds == Some(0) || self.data_bytes == Some(0) {
            return Err(PlanError::ZeroBudget);
        }
        if self.valid_seconds == 0 {
            return Err(PlanError::ZeroValidity);
        }
        if self.device_limit == 0 {
            return Err(PlanError::ZeroDeviceLimit);
        }
        if matches!(&self.fair_use, Some(fu) if fu.daily_bytes == 0) {
            return Err(PlanError::ZeroFairUseCap);
        }
        Ok(())
    }
}
