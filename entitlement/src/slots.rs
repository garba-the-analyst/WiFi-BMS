//! Device slots for single-device and multi-device plans.
//!
//! A plan allows `limit` devices. Each device takes a slot the first time it
//! connects. When every slot is taken, a new device must **swap** with an old
//! one, and swaps are limited to one per `swap_cooldown_secs` (24 hours by
//! default) so a code cannot be passed around all day.

use std::fmt;

use crate::UnixSecs;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub device_id: String,
    pub bound_at: UnixSecs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceSlots {
    pub limit: u8,
    pub swap_cooldown_secs: u32,
    pub bindings: Vec<Binding>,
    pub last_swap_at: Option<UnixSecs>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindOutcome {
    /// The device already held a slot.
    AlreadyBound,
    /// The device took a free slot.
    Bound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindError {
    /// Every slot is taken. `can_swap_now` says whether a swap would be allowed
    /// right now; if not, `next_swap_at` is when it will be.
    SlotsFull { can_swap_now: bool, next_swap_at: UnixSecs },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapError {
    /// The device to remove does not hold a slot.
    NotBound,
    /// The new device already holds a slot.
    AlreadyBound,
    /// A swap happened too recently.
    Cooldown { next_swap_at: UnixSecs },
}

impl fmt::Display for BindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BindError::SlotsFull { can_swap_now: true, .. } => write!(f, "all device slots are in use; swap a device to continue"),
            BindError::SlotsFull { next_swap_at, .. } => write!(f, "all device slots are in use; a swap is allowed from {next_swap_at}"),
        }
    }
}
impl std::error::Error for BindError {}

impl fmt::Display for SwapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SwapError::NotBound => write!(f, "that device does not hold a slot"),
            SwapError::AlreadyBound => write!(f, "the new device already holds a slot"),
            SwapError::Cooldown { next_swap_at } => write!(f, "a swap is allowed from {next_swap_at}"),
        }
    }
}
impl std::error::Error for SwapError {}

impl DeviceSlots {
    pub fn new(limit: u8, swap_cooldown_secs: u32) -> Self {
        DeviceSlots { limit, swap_cooldown_secs, bindings: Vec::new(), last_swap_at: None }
    }

    fn holds(&self, device_id: &str) -> bool {
        self.bindings.iter().any(|b| b.device_id == device_id)
    }

    /// When the next swap is allowed (now, if none is on cooldown).
    pub fn next_swap_at(&self, now: UnixSecs) -> UnixSecs {
        match self.last_swap_at {
            Some(t) => (t + i64::from(self.swap_cooldown_secs)).max(now),
            None => now,
        }
    }

    /// Give `device_id` a slot if it has none and one is free.
    pub fn bind(&mut self, device_id: &str, now: UnixSecs) -> Result<BindOutcome, BindError> {
        if self.holds(device_id) {
            return Ok(BindOutcome::AlreadyBound);
        }
        if self.bindings.len() < usize::from(self.limit) {
            self.bindings.push(Binding { device_id: device_id.to_owned(), bound_at: now });
            return Ok(BindOutcome::Bound);
        }
        let next = self.next_swap_at(now);
        Err(BindError::SlotsFull { can_swap_now: next <= now, next_swap_at: next })
    }

    /// Replace `old` with `new`, if the cooldown allows.
    pub fn swap(&mut self, old: &str, new: &str, now: UnixSecs) -> Result<(), SwapError> {
        if self.holds(new) {
            return Err(SwapError::AlreadyBound);
        }
        let Some(pos) = self.bindings.iter().position(|b| b.device_id == old) else {
            return Err(SwapError::NotBound);
        };
        let next = self.next_swap_at(now);
        if next > now {
            return Err(SwapError::Cooldown { next_swap_at: next });
        }
        self.bindings[pos] = Binding { device_id: new.to_owned(), bound_at: now };
        self.last_swap_at = Some(now);
        Ok(())
    }

    /// Free a slot without starting a cooldown (for example an admin action).
    pub fn release(&mut self, device_id: &str) -> bool {
        let before = self.bindings.len();
        self.bindings.retain(|b| b.device_id != device_id);
        self.bindings.len() != before
    }
}
