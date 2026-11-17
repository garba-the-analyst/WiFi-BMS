//! The agent's decisions, free of HTTP and of any real router, so they can be
//! tested exhaustively.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::central::{Central, CentralError};
use crate::control::{DeviceHints, RouterControl, SessionRef};
use crate::spool::Spool;
use crate::wire::{AccountingReply, AccountingReq, AuthorizeReply, AuthorizeReq};

/// The answer for FreeRADIUS's authorize step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Accept(Vec<(String, String)>),
    Reject(String),
    /// The platform could not be reached. FreeRADIUS must refuse the login;
    /// customers already online are not affected.
    Unavailable,
}

/// What we last told the router about one session, so we only send a CoA when something changed.
#[derive(Debug, Default, Clone)]
struct Mem {
    timeout: Option<u64>,
    rate: Option<String>,
}

pub struct Agent {
    central: Arc<dyn Central>,
    router: Arc<dyn RouterControl>,
    hints: Arc<dyn DeviceHints>,
    spool: Spool,
    sessions: Mutex<HashMap<String, Mem>>, // keyed by ticket username
}

impl Agent {
    pub fn new(central: Arc<dyn Central>, router: Arc<dyn RouterControl>, hints: Arc<dyn DeviceHints>, spool: Spool) -> Self {
        Agent { central, router, hints, spool, sessions: Mutex::new(HashMap::new()) }
    }

    pub fn spool_depth(&self) -> usize {
        self.spool.len()
    }

    pub async fn authorize(&self, mut req: AuthorizeReq) -> Decision {
        let h = self.hints.lookup(&req.mac).await;
        req.dhcp_hostname = h.hostname;
        req.dhcp_vendor_class = h.vendor_class;
        match self.central.authorize(&req).await {
            Ok(AuthorizeReply::Accept { attributes, .. }) => {
                // Remember what the router starts with, for change detection later.
                let mem = Mem {
                    timeout: attributes.iter().find(|(k, _)| k == "Session-Timeout").and_then(|(_, v)| v.parse().ok()),
                    rate: attributes.iter().find(|(k, _)| k == "Mikrotik-Rate-Limit").map(|(_, v)| v.clone()),
                };
                self.sessions.lock().expect("sessions lock").insert(req.username.clone(), mem);
                Decision::Accept(attributes)
            }
            Ok(AuthorizeReply::Reject { reason }) => Decision::Reject(reason),
            Err(CentralError::Rejected(code)) => Decision::Reject(format!("platform refused ({code})")),
            Err(e) => {
                tracing::warn!(error = %e, "authorize: platform unreachable");
                Decision::Unavailable
            }
        }
    }

    /// Handle one accounting event. Never fails towards FreeRADIUS: if the
    /// platform is down the event is queued, so the router is not held up.
    pub async fn accounting(&self, req: AccountingReq) {
        // Anything already waiting must go first, to keep events in order.
        if !self.spool.is_empty() {
            self.enqueue(req);
            self.replay().await;
            return;
        }
        match self.central.accounting(&req).await {
            Ok(reply) => self.apply(&req, &reply).await,
            Err(e) if e.is_retryable() => {
                tracing::warn!(error = %e, status = %req.status, "accounting: platform unreachable, queued");
                self.enqueue(req);
            }
            Err(e) => tracing::error!(error = %e, "accounting: platform rejected an update (dropped)"),
        }
    }

    fn enqueue(&self, req: AccountingReq) {
        if let Err(e) = self.spool.push(req) {
            // Out of disk or unwritable: the loudest thing we can do.
            tracing::error!(error = %e, "CANNOT WRITE THE OFFLINE QUEUE; usage will be lost");
        }
    }

    /// Send queued events oldest first. Stops at the first failure. Replies to
    /// replayed events are stale, so only the platform's records are updated;
    /// nothing is pushed to the router from them.
    pub async fn replay(&self) {
        while let Some(req) = self.spool.front() {
            match self.central.accounting(&req).await {
                Ok(_) => {}
                Err(e) if e.is_retryable() => return,
                Err(e) => tracing::error!(error = %e, "dropping a queued update the platform refused"),
            }
            if let Err(e) = self.spool.pop_front() {
                tracing::error!(error = %e, "cannot update the offline queue");
                return;
            }
        }
    }

    async fn apply(&self, req: &AccountingReq, reply: &AccountingReply) {
        let session = SessionRef { acct_session_id: req.acct_session_id.clone(), username: req.username.clone() };
        if req.status == "stop" {
            self.sessions.lock().expect("sessions lock").remove(&req.username);
            return;
        }
        if reply.action == "disconnect" {
            tracing::info!(user = %req.username, reason = ?reply.reason, "disconnecting");
            if let Err(e) = self.router.disconnect(&session).await {
                tracing::error!(error = %e, "router disconnect failed");
            }
            self.sessions.lock().expect("sessions lock").remove(&req.username);
            return;
        }
        if req.status != "interim" {
            return;
        }

        // The router's limit is total uptime; the platform answers with ACTIVE
        // seconds left. Idle minutes pass without costing plan time, so the
        // limit must move out by exactly the idle time.
        let new_timeout = reply.session_timeout_secs.map(|left| u64::from(req.session_secs) + u64::from(left));
        let (send_rate, send_timeout) = {
            let mut map = self.sessions.lock().expect("sessions lock");
            let mem = map.entry(req.username.clone()).or_default();
            let rate = match (&reply.rate_limit, &mem.rate) {
                (Some(new), Some(old)) if new == old => None,
                (Some(new), _) => Some(new.clone()),
                _ => None,
            };
            let timeout = match (new_timeout, mem.timeout) {
                (Some(new), Some(old)) if new == old => None,
                (Some(new), _) => Some(new),
                _ => None,
            };
            (rate, timeout)
        };
        if send_rate.is_none() && send_timeout.is_none() {
            return;
        }
        match self.router.set_limits(&session, send_rate.as_deref(), send_timeout).await {
            Ok(()) => {
                let mut map = self.sessions.lock().expect("sessions lock");
                let mem = map.entry(req.username.clone()).or_default();
                if send_rate.is_some() {
                    mem.rate = send_rate;
                }
                if send_timeout.is_some() {
                    mem.timeout = send_timeout;
                }
            }
            Err(e) => tracing::error!(error = %e, "router CoA failed; will retry on the next update"),
        }
    }

    /// Numbers for the admin Network screen.
    pub async fn send_metrics(&self, extra: &[(String, f64)]) {
        let mut m = vec![("agent_up".to_string(), 1.0), ("agent_spool_depth".to_string(), self.spool_depth() as f64)];
        m.extend_from_slice(extra);
        if let Err(e) = self.central.metrics(&m).await {
            tracing::debug!(error = %e, "metrics not sent");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{DhcpHints, NoHints};
    use async_trait::async_trait;
    use std::collections::VecDeque;

    #[derive(Default)]
    struct FakeCentral {
        auth: Mutex<Option<Result<AuthorizeReply, CentralError>>>,
        replies: Mutex<VecDeque<Result<AccountingReply, CentralError>>>,
        seen: Mutex<Vec<AccountingReq>>,
        seen_auth: Mutex<Vec<AuthorizeReq>>,
    }

    #[async_trait]
    impl Central for FakeCentral {
        async fn authorize(&self, req: &AuthorizeReq) -> Result<AuthorizeReply, CentralError> {
            self.seen_auth.lock().unwrap().push(req.clone());
            self.auth.lock().unwrap().take().expect("test set an authorize answer")
        }
        async fn accounting(&self, req: &AccountingReq) -> Result<AccountingReply, CentralError> {
            let r = self.replies.lock().unwrap().pop_front().unwrap_or_else(|| Ok(cont(None, None)));
            if r.is_ok() {
                self.seen.lock().unwrap().push(req.clone());
            }
            r
        }
        async fn metrics(&self, _: &[(String, f64)]) -> Result<(), CentralError> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeRouter {
        disconnects: Mutex<Vec<String>>,
        coas: Mutex<Vec<(Option<String>, Option<u64>)>>,
    }

    #[async_trait]
    impl RouterControl for FakeRouter {
        async fn disconnect(&self, s: &SessionRef) -> Result<(), String> {
            self.disconnects.lock().unwrap().push(s.username.clone());
            Ok(())
        }
        async fn set_limits(&self, _: &SessionRef, rate: Option<&str>, t: Option<u64>) -> Result<(), String> {
            self.coas.lock().unwrap().push((rate.map(str::to_string), t));
            Ok(())
        }
    }

    struct FixedHints;
    #[async_trait]
    impl DeviceHints for FixedHints {
        async fn lookup(&self, _: &str) -> DhcpHints {
            DhcpHints { hostname: Some("Amina-iPhone".into()), vendor_class: None }
        }
    }

    fn cont(rate: Option<&str>, timeout: Option<u32>) -> AccountingReply {
        AccountingReply { action: "continue".into(), reason: None, rate_limit: rate.map(str::to_string), session_timeout_secs: timeout }
    }

    fn acct(status: &str, secs: u32) -> AccountingReq {
        AccountingReq {
            status: status.into(),
            acct_session_id: "S1".into(),
            username: "tkabc".into(),
            mac: "a4:83:e7:12:34:56".into(),
            ip: None,
            bytes_in: u64::from(secs) * 1000,
            bytes_out: 0,
            session_secs: secs,
            ssid: None,
            terminate_cause: None,
            observation: None,
        }
    }

    fn agent(name: &str, hints: Arc<dyn DeviceHints>) -> (Agent, Arc<FakeCentral>, Arc<FakeRouter>) {
        let c = Arc::new(FakeCentral::default());
        let r = Arc::new(FakeRouter::default());
        let dir = std::env::temp_dir().join(format!("agent-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = Agent::new(c.clone(), r.clone(), hints, Spool::open(dir.join("s.jsonl"), 100).unwrap());
        (a, c, r)
    }

    fn auth_req() -> AuthorizeReq {
        AuthorizeReq { username: "tkabc".into(), password: "tp_x".into(), mac: "a4:83:e7:12:34:56".into(), dhcp_hostname: None, dhcp_vendor_class: None }
    }

    #[tokio::test]
    async fn authorize_passes_dhcp_hints_and_returns_the_platforms_limits() {
        let (a, c, _) = agent("auth", Arc::new(FixedHints));
        *c.auth.lock().unwrap() = Some(Ok(AuthorizeReply::Accept {
            entitlement_id: "e1".into(),
            attributes: vec![("Session-Timeout".into(), "3600".into()), ("Mikrotik-Rate-Limit".into(), "2000k/5000k".into())],
        }));
        let d = a.authorize(auth_req()).await;
        assert!(matches!(d, Decision::Accept(ref v) if v.len() == 2));
        assert_eq!(c.seen_auth.lock().unwrap()[0].dhcp_hostname.as_deref(), Some("Amina-iPhone"));
    }

    #[tokio::test]
    async fn authorize_reject_and_outage_are_told_apart() {
        let (a, c, _) = agent("rej", Arc::new(NoHints));
        *c.auth.lock().unwrap() = Some(Ok(AuthorizeReply::Reject { reason: "device limit reached for this plan".into() }));
        assert_eq!(a.authorize(auth_req()).await, Decision::Reject("device limit reached for this plan".into()));
        *c.auth.lock().unwrap() = Some(Err(CentralError::Unreachable("timeout".into())));
        assert_eq!(a.authorize(auth_req()).await, Decision::Unavailable);
        *c.auth.lock().unwrap() = Some(Err(CentralError::Rejected(401)));
        assert!(matches!(a.authorize(auth_req()).await, Decision::Reject(_)));
    }

    #[tokio::test]
    async fn a_disconnect_decision_reaches_the_router() {
        let (a, c, r) = agent("disc", Arc::new(NoHints));
        c.replies.lock().unwrap().push_back(Ok(AccountingReply { action: "disconnect".into(), reason: Some("idle".into()), rate_limit: None, session_timeout_secs: None }));
        a.accounting(acct("interim", 300)).await;
        assert_eq!(*r.disconnects.lock().unwrap(), vec!["tkabc".to_string()]);
    }

    #[tokio::test]
    async fn idle_time_pushes_the_routers_limit_out_by_exactly_the_idle_seconds() {
        let (a, c, r) = agent("idle", Arc::new(NoHints));
        // Session starts with 3600 active seconds left: the router is told Session-Timeout=3600.
        *c.auth.lock().unwrap() = Some(Ok(AuthorizeReply::Accept {
            entitlement_id: "e1".into(),
            attributes: vec![("Session-Timeout".into(), "3600".into()), ("Mikrotik-Rate-Limit".into(), "2000k/5000k".into())],
        }));
        a.authorize(auth_req()).await;

        // After an ACTIVE minute: 60 s online, 3540 left, so the limit is still 3600. No CoA needed.
        c.replies.lock().unwrap().push_back(Ok(cont(Some("2000k/5000k"), Some(3540))));
        a.accounting(acct("interim", 60)).await;
        assert!(r.coas.lock().unwrap().is_empty(), "nothing changed, nothing sent");

        // After an IDLE minute: 120 s online, still 3540 left, so the router may run until 3660.
        c.replies.lock().unwrap().push_back(Ok(cont(Some("2000k/5000k"), Some(3540))));
        a.accounting(acct("interim", 120)).await;
        assert_eq!(*r.coas.lock().unwrap(), vec![(None, Some(3660))]);

        // A repeat of the same answer sends nothing more.
        c.replies.lock().unwrap().push_back(Ok(cont(Some("2000k/5000k"), Some(3540 - 0))));
        a.accounting(acct("interim", 120)).await;
        assert_eq!(r.coas.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_speed_change_from_fair_use_is_pushed_once() {
        let (a, c, r) = agent("fup", Arc::new(NoHints));
        *c.auth.lock().unwrap() = Some(Ok(AuthorizeReply::Accept {
            entitlement_id: "e1".into(),
            attributes: vec![("Session-Timeout".into(), "86400".into()), ("Mikrotik-Rate-Limit".into(), "2000k/5000k".into())],
        }));
        a.authorize(auth_req()).await;
        c.replies.lock().unwrap().push_back(Ok(cont(Some("250k/500k"), Some(86340))));
        a.accounting(acct("interim", 60)).await;
        assert_eq!(*r.coas.lock().unwrap(), vec![(Some("250k/500k".to_string()), None)]);
        c.replies.lock().unwrap().push_back(Ok(cont(Some("250k/500k"), Some(86280))));
        a.accounting(acct("interim", 120)).await;
        assert_eq!(r.coas.lock().unwrap().len(), 1, "same speed again: no second CoA");
    }

    #[tokio::test]
    async fn outage_queues_in_order_and_replays_when_the_link_returns() {
        let (a, c, _) = agent("outage", Arc::new(NoHints));
        for _ in 0..3 {
            c.replies.lock().unwrap().push_back(Err(CentralError::Unreachable("down".into())));
        }
        a.accounting(acct("start", 0)).await; // fails, queued
        assert_eq!(a.spool_depth(), 1);
        a.accounting(acct("interim", 60)).await; // queue not empty: appended, replay tried and fails again
        a.accounting(acct("interim", 120)).await;
        assert_eq!(a.spool_depth(), 3);
        assert!(c.seen.lock().unwrap().is_empty(), "nothing delivered during the outage");

        // Link returns.
        a.accounting(acct("stop", 180)).await;
        assert_eq!(a.spool_depth(), 0);
        let order: Vec<_> = c.seen.lock().unwrap().iter().map(|r| (r.status.clone(), r.session_secs)).collect();
        assert_eq!(order, vec![("start".into(), 0), ("interim".into(), 60), ("interim".into(), 120), ("stop".into(), 180)], "oldest first");
    }

    #[tokio::test]
    async fn replayed_answers_never_disconnect_or_change_the_router() {
        let (a, c, r) = agent("stale", Arc::new(NoHints));
        c.replies.lock().unwrap().push_back(Err(CentralError::Unreachable("down".into())));
        a.accounting(acct("interim", 60)).await;
        // On replay the platform says "disconnect" about the old event; the session is long gone.
        c.replies.lock().unwrap().push_back(Ok(AccountingReply { action: "disconnect".into(), reason: Some("idle".into()), rate_limit: None, session_timeout_secs: None }));
        a.replay().await;
        assert_eq!(a.spool_depth(), 0);
        assert!(r.disconnects.lock().unwrap().is_empty() && r.coas.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_refused_update_is_dropped_not_retried_forever() {
        let (a, c, _) = agent("refused", Arc::new(NoHints));
        c.replies.lock().unwrap().push_back(Err(CentralError::Rejected(400)));
        a.accounting(acct("interim", 60)).await;
        assert_eq!(a.spool_depth(), 0);
    }
}
