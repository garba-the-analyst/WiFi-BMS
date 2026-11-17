//! Site agent: the part of the platform that lives at each Starlink site.
//!
//! ```text
//!  MikroTik hotspot ──RADIUS──▶ FreeRADIUS ──HTTP (rlm_rest)──▶ site agent ──HTTPS──▶ central platform
//!        ▲                                                           │
//!        └──────── CoA / Disconnect (radclient, port 3799) ◀─────────┘
//! ```
//!
//! What it does:
//! * **authorize**: passes the hotspot's login (a one-time ticket) to the
//!   platform and turns the answer into RADIUS reply attributes.
//! * **accounting**: forwards Start / Interim / Stop, and carries out the
//!   platform's decision: disconnect, change speed, refresh the time limit.
//! * **keeps working offline**: if the internet link or the platform is
//!   unreachable, accounting is queued on disk and replayed in order when
//!   the link returns. Customers already online stay online; new logins wait.
//!
//! One rule explains the time-limit handling: the router's `Session-Timeout`
//! counts WALL-CLOCK seconds since the session began, but a plan counts
//! ACTIVE seconds only. After every update the agent therefore sets the
//! router's limit to `seconds already online + active seconds left`, so idle
//! time never uses up a customer's plan.

pub mod central;
pub mod config;
pub mod control;
pub mod radius_map;
pub mod server;
pub mod service;
pub mod spool;
pub mod wire;
