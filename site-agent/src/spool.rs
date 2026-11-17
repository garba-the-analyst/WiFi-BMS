//! The offline queue.
//!
//! When the platform cannot be reached, accounting updates are appended to a
//! file (one JSON object per line) and replayed in order later. Surviving a
//! restart matters: a power cut during an outage must not lose usage.
//!
//! Interim updates carry running totals, so only the LATEST one per session
//! is needed. When the file grows past the cap we keep every start and stop
//! and the newest interim of each session, which loses nothing.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::wire::AccountingReq;

pub struct Spool {
    path: PathBuf,
    cap: usize,
    items: Mutex<Vec<AccountingReq>>,
}

impl Spool {
    /// Open (and load) the spool file, creating its folder if needed.
    pub fn open(path: impl Into<PathBuf>, cap: usize) -> std::io::Result<Self> {
        let path = path.into();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut items = Vec::new();
        if let Ok(text) = fs::read_to_string(&path) {
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                match serde_json::from_str::<AccountingReq>(line) {
                    Ok(r) => items.push(r),
                    // A half-written last line after a power cut is expected; skip it.
                    Err(e) => tracing::warn!(error = %e, "skipping unreadable spool line"),
                }
            }
        }
        Ok(Spool { path, cap: cap.max(10), items: Mutex::new(items) })
    }

    pub fn len(&self) -> usize {
        self.items.lock().expect("spool lock").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Add to the end of the queue and write it to disk.
    pub fn push(&self, req: AccountingReq) -> std::io::Result<()> {
        let mut items = self.items.lock().expect("spool lock");
        let line = serde_json::to_string(&req).expect("request serialises");
        let mut f = fs::OpenOptions::new().create(true).append(true).open(&self.path)?;
        writeln!(f, "{line}")?;
        f.sync_data()?;
        items.push(req);
        if items.len() > self.cap {
            Self::compact(&mut items);
            self.rewrite(&items)?;
        }
        Ok(())
    }

    /// The oldest entry, without removing it.
    pub fn front(&self) -> Option<AccountingReq> {
        self.items.lock().expect("spool lock").first().cloned()
    }

    /// Remove the oldest entry once it has been delivered.
    pub fn pop_front(&self) -> std::io::Result<()> {
        let mut items = self.items.lock().expect("spool lock");
        if !items.is_empty() {
            items.remove(0);
            self.rewrite(&items)?;
        }
        Ok(())
    }

    fn rewrite(&self, items: &[AccountingReq]) -> std::io::Result<()> {
        let tmp = self.path.with_extension("tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            for r in items {
                writeln!(f, "{}", serde_json::to_string(r).expect("request serialises"))?;
            }
            f.sync_data()?;
        }
        fs::rename(tmp, &self.path) // atomic swap: a crash leaves the old or the new file, never half
    }

    /// Keep starts and stops, and only the newest interim per session.
    fn compact(items: &mut Vec<AccountingReq>) {
        let mut newest_interim: HashMap<String, usize> = HashMap::new();
        for (i, r) in items.iter().enumerate() {
            if r.status == "interim" {
                newest_interim.insert(r.acct_session_id.clone(), i);
            }
        }
        let mut i = 0;
        items.retain(|r| {
            let keep = r.status != "interim" || newest_interim.get(&r.acct_session_id) == Some(&i);
            i += 1;
            keep
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(status: &str, sid: &str, secs: u32) -> AccountingReq {
        AccountingReq {
            status: status.into(),
            acct_session_id: sid.into(),
            username: "tk".into(),
            mac: "aa:bb:cc:dd:ee:ff".into(),
            ip: None,
            bytes_in: 1,
            bytes_out: 2,
            session_secs: secs,
            ssid: None,
            terminate_cause: None,
            observation: None,
        }
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("spool-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d.join("spool.jsonl")
    }

    #[test]
    fn keeps_order_and_survives_a_restart() {
        let p = tmp("restart");
        let s = Spool::open(&p, 100).unwrap();
        s.push(req("start", "S1", 0)).unwrap();
        s.push(req("interim", "S1", 60)).unwrap();
        drop(s);
        let s = Spool::open(&p, 100).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s.front().unwrap().status, "start");
        s.pop_front().unwrap();
        assert_eq!(s.front().unwrap().session_secs, 60);
        drop(s);
        assert_eq!(Spool::open(&p, 100).unwrap().len(), 1, "removal is persisted too");
    }

    #[test]
    fn a_torn_last_line_is_skipped() {
        let p = tmp("torn");
        let s = Spool::open(&p, 100).unwrap();
        s.push(req("start", "S1", 0)).unwrap();
        drop(s);
        fs::OpenOptions::new().append(true).open(&p).unwrap().write_all(b"{\"status\":\"inter").unwrap();
        assert_eq!(Spool::open(&p, 100).unwrap().len(), 1);
    }

    #[test]
    fn compaction_keeps_starts_stops_and_the_newest_interim() {
        let p = tmp("compact");
        let s = Spool::open(&p, 10).unwrap();
        s.push(req("start", "S1", 0)).unwrap();
        for i in 1..=20 {
            s.push(req("interim", "S1", 60 * i)).unwrap();
        }
        s.push(req("stop", "S1", 1300)).unwrap();
        assert!(s.len() <= 11, "compacted, got {}", s.len());
        let mut kinds = Vec::new();
        while let Some(r) = s.front() {
            kinds.push((r.status.clone(), r.session_secs));
            s.pop_front().unwrap();
        }
        assert_eq!(kinds.first().unwrap().0, "start");
        assert_eq!(kinds.last().unwrap().0, "stop");
        assert!(kinds.iter().any(|(k, secs)| k == "interim" && *secs == 1200), "newest interim survives: {kinds:?}");
    }
}
