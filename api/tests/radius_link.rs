//! Tests of the router link: access code → ticket → RADIUS authorize →
//! accounting, against a real PostgreSQL with row-level security on.
//! See `run_integration.sh` for the setup. Without the environment variables
//! every test prints a notice and passes.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use entitlement::{FairUse, Plan, PlanKind, SpeedProfile};
use http_body_util::BodyExt;
use platform_api::auth::{sha256_hex, RateLimiter};
use platform_api::error::{AppError, Result};
use platform_api::http::router;
use platform_api::paystack::{Gateway, InitializeRequest, InitializedTransaction, VerifiedTransaction};
use platform_api::secrets::MasterKey;
use platform_api::{AppState, Config};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use tower::ServiceExt;
use uuid::Uuid;

struct NoGateway;
#[async_trait]
impl Gateway for NoGateway {
    async fn initialize(&self, _: InitializeRequest<'_>) -> Result<InitializedTransaction> {
        Err(AppError::Gateway("not used in these tests".into()))
    }
    async fn verify(&self, _: &str, _: &str) -> Result<VerifiedTransaction> {
        Err(AppError::Gateway("not used in these tests".into()))
    }
}

const UA_ANDROID: &str = "Mozilla/5.0 (Linux; Android 14; SM-S911B) AppleWebKit/537.36 Chrome/120.0 Mobile Safari/537.36";
const MAC1: &str = "A4:83:E7:12:34:56";
const MAC2: &str = "3C:22:FB:00:00:01";

struct World {
    admin: PgPool,
    state: AppState,
    slug: String,
    tenant: Uuid,
    site_id: Uuid,
    plan_id: Uuid,
    token: String,
    code_n: std::sync::atomic::AtomicU32,
}

fn speed(down: u32, up: u32) -> SpeedProfile {
    SpeedProfile { down_kbps: down, up_kbps: up, burst: None, priority: 8 }
}

fn plan(kind: PlanKind, secs: Option<u32>, bytes: Option<u64>) -> Plan {
    Plan {
        kind,
        active_seconds: secs,
        data_bytes: bytes,
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

async fn world() -> Option<World> {
    let (Ok(app_url), Ok(admin_url)) = (std::env::var("DATABASE_URL"), std::env::var("ADMIN_DATABASE_URL")) else {
        eprintln!("NOTICE: DATABASE_URL / ADMIN_DATABASE_URL not set; skipping integration test");
        return None;
    };
    let app = PgPoolOptions::new().max_connections(5).connect(&app_url).await.unwrap();
    let admin = PgPoolOptions::new().max_connections(2).connect(&admin_url).await.unwrap();
    let slug = format!("r-{}", &Uuid::new_v4().simple().to_string()[..10]);
    let tenant: Uuid = sqlx::query_scalar("INSERT INTO tenants (name, slug) VALUES ($1, $1) RETURNING id").bind(&slug).fetch_one(&admin).await.unwrap();
    let site_id: Uuid = sqlx::query_scalar("INSERT INTO sites (tenant_id, name, slug) VALUES ($1, 'Site 1', 'site-1') RETURNING id")
        .bind(tenant)
        .fetch_one(&admin)
        .await
        .unwrap();
    let profile: Uuid = sqlx::query_scalar("INSERT INTO speed_profiles (tenant_id, name, down_kbps, up_kbps) VALUES ($1, 'Std', 5000, 2000) RETURNING id")
        .bind(tenant)
        .fetch_one(&admin)
        .await
        .unwrap();
    let plan_id: Uuid = sqlx::query_scalar(
        "INSERT INTO plans (tenant_id, name, kind, price_kobo, active_seconds, valid_seconds, speed_profile_id) VALUES ($1, 'Hourly', 'time', 10000, 3600, 86400, $2) RETURNING id",
    )
    .bind(tenant)
    .bind(profile)
    .fetch_one(&admin)
    .await
    .unwrap();
    let token = format!("nt_{}", Uuid::new_v4().simple());
    sqlx::query("INSERT INTO node_tokens (tenant_id, site_id, name, token_hash) VALUES ($1, $2, 'test', $3)")
        .bind(tenant)
        .bind(site_id)
        .bind(sha256_hex(&token))
        .execute(&admin)
        .await
        .unwrap();
    let state = AppState {
        pool: app,
        gateway: Arc::new(NoGateway),
        master_key: MasterKey::from_hex(&"00".repeat(32)).unwrap(),
        config: Config { public_base_url: "https://wifi.test".into(), fallback_email_domain: "guest.test".into(), trust_proxy: false },
        limiter: Arc::new(RateLimiter::new()),
        sms: Arc::new(platform_api::sms::LogSms),
    };
    Some(World { admin, state, slug, tenant, site_id, plan_id, token, code_n: Default::default() })
}

impl World {
    /// A guest code that owns one fresh purchase of `p`. Returns the code text.
    async fn buy(&self, p: &Plan) -> String {
        let n = self.code_n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let code = format!("ABCD{:04}", 2000 + n).replace('0', "2").replace('1', "3");
        let code_id: Uuid = sqlx::query_scalar("INSERT INTO access_codes (tenant_id, code, phone) VALUES ($1, $2, '+2348012345678') RETURNING id")
            .bind(self.tenant)
            .bind(&code)
            .fetch_one(&self.admin)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO entitlements (tenant_id, plan_id, plan_snapshot, access_code_id, status, expires_at, remaining_active_seconds, remaining_bytes)
             VALUES ($1, $2, $3, $4, 'pending', now() + interval '90 days', $5, $6)",
        )
        .bind(self.tenant)
        .bind(self.plan_id)
        .bind(serde_json::to_value(p).unwrap())
        .bind(code_id)
        .bind(p.active_seconds.map(|v| v as i32))
        .bind(p.data_bytes.map(|v| v as i64))
        .execute(&self.admin)
        .await
        .unwrap();
        code
    }

    async fn call(&self, req: Request<Body>) -> (StatusCode, Value) {
        let resp = router(self.state.clone()).oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn connect(&self, code: &str, mac: &str) -> (StatusCode, Value) {
        let body = json!({ "site": "site-1", "mac": mac, "code": code, "accept_terms": true });
        self.call(
            Request::post(format!("/v1/t/{}/connect", self.slug))
                .header("content-type", "application/json")
                .header("user-agent", UA_ANDROID)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
    }

    async fn node(&self, path: &str, token: &str, body: Value) -> (StatusCode, Value) {
        self.call(
            Request::post(format!("/v1/node/radius/{path}"))
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
    }

    async fn authorize(&self, ticket: &Value, mac: &str) -> Value {
        let (s, v) = self
            .node(
                "authorize",
                &self.token,
                json!({ "username": ticket["username"], "password": ticket["password"], "mac": mac, "dhcp_vendor_class": "android-dhcp-14" }),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v
    }

    /// Connect and authorize in one go; returns (username, entitlement id).
    async fn online(&self, code: &str, mac: &str) -> (String, Uuid) {
        let (s, ticket) = self.connect(code, mac).await;
        assert_eq!(s, StatusCode::OK, "{ticket}");
        let a = self.authorize(&ticket, mac).await;
        assert_eq!(a["result"], "accept", "{a}");
        (ticket["username"].as_str().unwrap().to_string(), a["entitlement_id"].as_str().unwrap().parse().unwrap())
    }

    #[allow(clippy::too_many_arguments)]
    async fn acct(&self, status: &str, sid: &str, user: &str, mac: &str, bytes_in: u64, bytes_out: u64, secs: u32, extra: Value) -> Value {
        let mut body = json!({ "status": status, "acct_session_id": sid, "username": user, "mac": mac, "ip": "10.5.50.20",
                               "bytes_in": bytes_in, "bytes_out": bytes_out, "session_secs": secs });
        if let Value::Object(m) = extra {
            for (k, v) in m {
                body[k] = v;
            }
        }
        let (s, v) = self.node("accounting", &self.token, body).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v
    }

    async fn ent(&self, id: Uuid) -> (String, Option<i32>, Option<i64>) {
        let r = sqlx::query("SELECT status::text AS s, remaining_active_seconds AS a, remaining_bytes AS b FROM entitlements WHERE id = $1")
            .bind(id)
            .fetch_one(&self.admin)
            .await
            .unwrap();
        (r.get("s"), r.get("a"), r.get("b"))
    }

    async fn end_reason(&self, sid: &str) -> Option<String> {
        sqlx::query_scalar("SELECT end_reason::text FROM sessions WHERE acct_session_id = $1 AND tenant_id = $2")
            .bind(sid)
            .bind(self.tenant)
            .fetch_one(&self.admin)
            .await
            .unwrap()
    }
}

const ACTIVE: u64 = 200_000; // well above the 20 KB activity threshold

#[tokio::test]
async fn a_guest_connects_and_active_minutes_are_charged() {
    let Some(w) = world().await else { return };
    let code = w.buy(&plan(PlanKind::Time, Some(3600), None)).await;
    let (user, ent_id) = w.online(&code, MAC1).await;

    // Authorisation activated the plan and set the router's limits.
    let (status, remaining, _) = w.ent(ent_id).await;
    assert_eq!(status, "active");
    assert_eq!(remaining, Some(3600));

    let (_, ticket) = w.connect(&code, MAC2).await; // second ticket just to read attributes below
    assert!(ticket["username"].is_string());

    assert_eq!(w.acct("start", "S1", &user, MAC1, 0, 0, 0, json!({})).await["action"], "continue");
    let r = w.acct("interim", "S1", &user, MAC1, ACTIVE / 2, ACTIVE / 2, 60, json!({})).await;
    assert_eq!(r["action"], "continue");
    assert_eq!(r["rate_limit"], "2000k/5000k", "upload/download order for MikroTik");
    assert_eq!(r["session_timeout_secs"], 3540);
    assert_eq!(w.ent(ent_id).await.1, Some(3540), "one active minute charged");

    // A minute of idle chatter is NOT charged.
    w.acct("interim", "S1", &user, MAC1, ACTIVE / 2 + 100, ACTIVE / 2, 120, json!({})).await;
    assert_eq!(w.ent(ent_id).await.1, Some(3540), "idle minute is free");

    let ledger: i64 = sqlx::query_scalar("SELECT coalesce(sum(amount),0)::bigint FROM usage_ledger WHERE entitlement_id = $1 AND kind = 'active_seconds'")
        .bind(ent_id)
        .fetch_one(&w.admin)
        .await
        .unwrap();
    assert_eq!(ledger, 60);

    // Device was recorded with what we could tell about it.
    let d = sqlx::query("SELECT os_family, os_version, device_class, mac_randomised FROM devices WHERE tenant_id = $1 AND mac = $2::macaddr")
        .bind(w.tenant)
        .bind(MAC1)
        .fetch_one(&w.admin)
        .await
        .unwrap();
    assert_eq!(d.get::<Option<String>, _>("os_family").as_deref(), Some("Android"));
    assert_eq!(d.get::<Option<String>, _>("os_version").as_deref(), Some("14"));
    assert_eq!(d.get::<String, _>("device_class"), "phone");
    assert!(!d.get::<bool, _>("mac_randomised"));

    // Stop closes the session.
    w.acct("stop", "S1", &user, MAC1, ACTIVE, ACTIVE, 180, json!({ "terminate_cause": "User-Request" })).await;
    assert_eq!(w.end_reason("S1").await.as_deref(), Some("user"));
    // Consent and audit trail exist.
    let consents: i64 = sqlx::query_scalar("SELECT count(*) FROM consents WHERE tenant_id = $1").bind(w.tenant).fetch_one(&w.admin).await.unwrap();
    assert!(consents >= 1);
    let broken: i64 = sqlx::query_scalar("SELECT count(*) FROM verify_audit_chain($1)").bind(w.tenant).fetch_one(&w.admin).await.unwrap();
    assert_eq!(broken, 0);
}

#[tokio::test]
async fn an_idle_device_is_disconnected_without_losing_time() {
    let Some(w) = world().await else { return };
    let code = w.buy(&plan(PlanKind::Time, Some(3600), None)).await;
    let (user, ent_id) = w.online(&code, MAC1).await;
    w.acct("start", "S2", &user, MAC1, 0, 0, 0, json!({})).await;

    let mut last = json!({});
    for i in 1..=5u32 {
        last = w.acct("interim", "S2", &user, MAC1, 100 * u64::from(i), 100 * u64::from(i), 60 * i, json!({})).await;
    }
    assert_eq!(last["action"], "disconnect", "5 idle minutes reaches the 300 s timeout");
    assert_eq!(w.end_reason("S2").await.as_deref(), Some("idle"));
    assert_eq!(w.ent(ent_id).await, ("active".to_string(), Some(3600), None), "idle time costs nothing and the plan stays usable");
}

#[tokio::test]
async fn running_out_of_time_disconnects_and_blocks_reconnection() {
    let Some(w) = world().await else { return };
    let code = w.buy(&plan(PlanKind::Time, Some(120), None)).await;
    let (user, ent_id) = w.online(&code, MAC1).await;
    w.acct("start", "S3", &user, MAC1, 0, 0, 0, json!({})).await;
    assert_eq!(w.acct("interim", "S3", &user, MAC1, ACTIVE, 0, 60, json!({})).await["action"], "continue");
    let r = w.acct("interim", "S3", &user, MAC1, 2 * ACTIVE, 0, 120, json!({})).await;
    assert_eq!(r["action"], "disconnect");
    assert_eq!(w.end_reason("S3").await.as_deref(), Some("time_exhausted"));
    assert_eq!(w.ent(ent_id).await.0, "exhausted");

    let (s, v) = w.connect(&code, MAC1).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert!(v["error"].as_str().unwrap().contains("no time or data left"));
}

#[tokio::test]
async fn a_data_plan_cuts_off_exactly_at_its_budget() {
    let Some(w) = world().await else { return };
    let code = w.buy(&plan(PlanKind::Data, None, Some(1_000_000))).await;
    let (user, ent_id) = w.online(&code, MAC1).await;
    w.acct("start", "S4", &user, MAC1, 0, 0, 0, json!({})).await;
    assert_eq!(w.acct("interim", "S4", &user, MAC1, 300_000, 300_000, 60, json!({})).await["action"], "continue");
    assert_eq!(w.ent(ent_id).await.2, Some(400_000));
    let r = w.acct("interim", "S4", &user, MAC1, 600_000, 600_000, 120, json!({})).await;
    assert_eq!(r["action"], "disconnect");
    assert_eq!(w.end_reason("S4").await.as_deref(), Some("data_exhausted"));
    assert_eq!(w.ent(ent_id).await.2, Some(0), "never charged beyond what was bought");
}

#[tokio::test]
async fn a_ticket_works_once_for_its_own_device_only() {
    let Some(w) = world().await else { return };
    let code = w.buy(&plan(PlanKind::Time, Some(3600), None)).await;
    let (_, ticket) = w.connect(&code, MAC1).await;

    let wrong_mac = w.node("authorize", &w.token, json!({ "username": ticket["username"], "password": ticket["password"], "mac": MAC2 })).await.1;
    assert_eq!(wrong_mac["result"], "reject", "ticket is bound to the MAC it was issued for");
    let wrong_pw = w.node("authorize", &w.token, json!({ "username": ticket["username"], "password": "tp_guess", "mac": MAC1 })).await.1;
    assert_eq!(wrong_pw["result"], "reject");
    assert_eq!(w.authorize(&ticket, MAC1).await["result"], "accept");
    assert_eq!(w.authorize(&ticket, MAC1).await["result"], "reject", "a ticket cannot be replayed");
    let unknown = w.node("authorize", &w.token, json!({ "username": "tkdoesnotexist", "password": "x", "mac": MAC1 })).await.1;
    assert_eq!(unknown["result"], "reject");
}

#[tokio::test]
async fn a_single_device_plan_refuses_a_second_device_and_records_it() {
    let Some(w) = world().await else { return };
    let code = w.buy(&plan(PlanKind::Time, Some(3600), None)).await;
    w.online(&code, MAC1).await;
    let (s, ticket) = w.connect(&code, MAC2).await;
    assert_eq!(s, StatusCode::OK);
    let a = w.authorize(&ticket, MAC2).await;
    assert_eq!(a["result"], "reject");
    assert!(a["reason"].as_str().unwrap().contains("device limit"));
    let flags: i64 = sqlx::query_scalar("SELECT count(*) FROM tethering_flags WHERE tenant_id = $1 AND signal = 'slot_overflow'")
        .bind(w.tenant)
        .fetch_one(&w.admin)
        .await
        .unwrap();
    assert_eq!(flags, 1);

    // A three-device plan takes both.
    let mut multi = plan(PlanKind::Time, Some(3600), None);
    multi.device_limit = 3;
    let code3 = w.buy(&multi).await;
    w.online(&code3, "AA:BB:CC:DD:EE:01").await;
    w.online(&code3, "AA:BB:CC:DD:EE:02").await;
}

#[tokio::test]
async fn hotspot_sharing_signals_cut_a_session_but_one_signal_does_not() {
    let Some(w) = world().await else { return };
    let code = w.buy(&plan(PlanKind::Time, Some(3600), None)).await;
    let (user, _) = w.online(&code, MAC1).await;
    w.acct("start", "S5", &user, MAC1, 0, 0, 0, json!({})).await;

    // TTL alone: flagged, not cut.
    let r = w.acct("interim", "S5", &user, MAC1, ACTIVE, 0, 60, json!({ "observation": { "ttl_lowered_ratio": 1.0 } })).await;
    assert_eq!(r["action"], "continue");

    // TTL plus mixed operating systems: cut.
    let r = w.acct("interim", "S5", &user, MAC1, 2 * ACTIVE, 0, 120, json!({ "observation": { "ttl_lowered_ratio": 0.9, "distinct_os_families": 2 } })).await;
    assert_eq!(r["action"], "disconnect");
    assert_eq!(w.end_reason("S5").await.as_deref(), Some("tethering"));
    let flags: i64 = sqlx::query_scalar("SELECT count(*) FROM tethering_flags WHERE tenant_id = $1 AND action IN ('warn','disconnect')")
        .bind(w.tenant)
        .fetch_one(&w.admin)
        .await
        .unwrap();
    assert!(flags >= 1);
}

#[tokio::test]
async fn fair_use_slows_the_session_after_the_daily_cap() {
    let Some(w) = world().await else { return };
    let mut p = plan(PlanKind::Time, Some(86_400), None);
    p.fair_use = Some(FairUse { daily_bytes: 100_000, throttled: speed(500, 250) });
    let code = w.buy(&p).await;
    let (user, _) = w.online(&code, MAC1).await;
    w.acct("start", "S6", &user, MAC1, 0, 0, 0, json!({})).await;
    let before = w.acct("interim", "S6", &user, MAC1, 30_000, 0, 60, json!({})).await;
    assert_eq!(before["rate_limit"], "2000k/5000k");
    let after = w.acct("interim", "S6", &user, MAC1, 150_000, 0, 120, json!({})).await;
    assert_eq!(after["action"], "continue");
    assert_eq!(after["rate_limit"], "250k/500k", "slowed, not cut off");
}

#[tokio::test]
async fn router_counter_restarts_do_not_double_charge_or_go_negative() {
    let Some(w) = world().await else { return };
    let code = w.buy(&plan(PlanKind::Data, None, Some(10_000_000))).await;
    let (user, ent_id) = w.online(&code, MAC1).await;
    w.acct("start", "S7", &user, MAC1, 0, 0, 0, json!({})).await;
    w.acct("interim", "S7", &user, MAC1, 500_000, 0, 60, json!({})).await;
    // The router reboots and its counters start again from a small number.
    w.acct("interim", "S7", &user, MAC1, 50_000, 0, 20, json!({})).await;
    assert_eq!(w.ent(ent_id).await.2, Some(10_000_000 - 550_000));
}

#[tokio::test]
async fn only_a_valid_node_token_may_use_the_node_endpoints() {
    let Some(w) = world().await else { return };
    let body = json!({ "username": "x", "password": "y", "mac": MAC1 });
    assert_eq!(w.node("authorize", "nt_wrong", body.clone()).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(w.node("authorize", "not-a-node-token", body.clone()).await.0, StatusCode::UNAUTHORIZED);
    let no_header = Request::post("/v1/node/radius/authorize").header("content-type", "application/json").body(Body::from(body.to_string())).unwrap();
    assert_eq!(w.call(no_header).await.0, StatusCode::UNAUTHORIZED);

    sqlx::query("UPDATE node_tokens SET revoked_at = now() WHERE tenant_id = $1").bind(w.tenant).execute(&w.admin).await.unwrap();
    assert_eq!(w.node("authorize", &w.token, body).await.0, StatusCode::UNAUTHORIZED, "revoked tokens stop working at once");
}

#[tokio::test]
async fn code_guessing_is_refused_and_logged() {
    let Some(w) = world().await else { return };
    for _ in 0..3 {
        let (s, _) = w.connect("ZZZZ9999", MAC1).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }
    let logged: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND action = 'auth.code_rejected'")
        .bind(w.tenant)
        .fetch_one(&w.admin)
        .await
        .unwrap();
    assert_eq!(logged, 3);
    // The same code hammered past the limit is blocked.
    let mut last = StatusCode::OK;
    for _ in 0..12 {
        last = w.connect("ZZZZ9999", MAC1).await.0;
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
    // Terms must be accepted.
    let body = json!({ "site": "site-1", "mac": MAC1, "code": "ABCD2345", "accept_terms": false });
    let (s, _) =
        w.call(Request::post(format!("/v1/t/{}/connect", w.slug)).header("content-type", "application/json").body(Body::from(body.to_string())).unwrap()).await;
    assert!(s == StatusCode::BAD_REQUEST || s == StatusCode::TOO_MANY_REQUESTS);
    let _ = w.site_id;
}
