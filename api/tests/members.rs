//! Member sign-in, wallet and devices against a real PostgreSQL with row-level
//! security on. See `run_integration.sh`. Without the environment variables
//! every test prints a notice and passes.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use platform_api::auth::RateLimiter;
use platform_api::error::Result;
use platform_api::fulfil::{fulfil_payment, Fulfilment};
use platform_api::http::router;
use platform_api::paystack::{Gateway, InitializeRequest, InitializedTransaction, VerifiedTransaction};
use platform_api::secrets::MasterKey;
use platform_api::sms::SmsSender;
use platform_api::{AppState, Config};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

struct OkGateway;
#[async_trait]
impl Gateway for OkGateway {
    async fn initialize(&self, r: InitializeRequest<'_>) -> Result<InitializedTransaction> {
        Ok(InitializedTransaction { authorization_url: format!("https://checkout.test/{}", r.reference), reference: r.reference.to_string() })
    }
    async fn verify(&self, _: &str, _: &str) -> Result<VerifiedTransaction> {
        unreachable!("not used")
    }
}

/// Remembers the last text so tests can read the sign-in code.
#[derive(Default)]
struct CaptureSms(Mutex<Vec<(String, String)>>);
#[async_trait]
impl SmsSender for CaptureSms {
    async fn send(&self, phone: &str, text: &str) -> Result<()> {
        self.0.lock().unwrap().push((phone.into(), text.into()));
        Ok(())
    }
}

struct World {
    admin: PgPool,
    state: AppState,
    sms: Arc<CaptureSms>,
    slug: String,
    tenant: Uuid,
    plan_id: Uuid,
}

async fn world() -> Option<World> {
    let (Ok(app_url), Ok(admin_url)) = (std::env::var("DATABASE_URL"), std::env::var("ADMIN_DATABASE_URL")) else {
        eprintln!("NOTICE: DATABASE_URL / ADMIN_DATABASE_URL not set; skipping integration test");
        return None;
    };
    let app = PgPoolOptions::new().max_connections(5).connect(&app_url).await.unwrap();
    let admin = PgPoolOptions::new().max_connections(2).connect(&admin_url).await.unwrap();
    let slug = format!("m-{}", &Uuid::new_v4().simple().to_string()[..10]);
    let tenant: Uuid = sqlx::query_scalar("INSERT INTO tenants (name, slug) VALUES ($1, $1) RETURNING id").bind(&slug).fetch_one(&admin).await.unwrap();
    let site: Uuid = sqlx::query_scalar("INSERT INTO sites (tenant_id, name, slug) VALUES ($1, 'Site 1', 'site-1') RETURNING id")
        .bind(tenant)
        .fetch_one(&admin)
        .await
        .unwrap();
    let _ = site;
    let profile: Uuid = sqlx::query_scalar("INSERT INTO speed_profiles (tenant_id, name, down_kbps, up_kbps) VALUES ($1, 'Std', 5000, 2000) RETURNING id")
        .bind(tenant)
        .fetch_one(&admin)
        .await
        .unwrap();
    let plan_id: Uuid = sqlx::query_scalar(
        "INSERT INTO plans (tenant_id, name, kind, price_kobo, active_seconds, valid_seconds, speed_profile_id, device_limit, swap_cooldown_s)
         VALUES ($1, 'Hourly', 'time', 10000, 3600, 86400, $2, 1, 86400) RETURNING id",
    )
    .bind(tenant)
    .bind(profile)
    .fetch_one(&admin)
    .await
    .unwrap();
    let key = MasterKey::from_hex(&"00".repeat(32)).unwrap();
    sqlx::query("INSERT INTO tenant_secrets (tenant_id, paystack_secret_enc) VALUES ($1, $2)")
        .bind(tenant)
        .bind(key.encrypt("sk_test_x").unwrap())
        .execute(&admin)
        .await
        .unwrap();
    let sms = Arc::new(CaptureSms::default());
    let state = AppState {
        pool: app,
        gateway: Arc::new(OkGateway),
        master_key: key,
        config: Config { public_base_url: "https://wifi.test".into(), fallback_email_domain: "guest.test".into(), trust_proxy: false },
        limiter: Arc::new(RateLimiter::new()),
        sms: sms.clone(),
    };
    Some(World { admin, state, sms, slug, tenant, plan_id })
}

impl World {
    async fn call(&self, method: &str, path: &str, token: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
        let mut b = Request::builder()
            .method(method)
            .uri(format!("/v1/t/{}/members{}", self.slug, path))
            .header("content-type", "application/json")
            .header("user-agent", "Mozilla/5.0 (Linux; Android 14; SM-S911B) Chrome/120 Mobile Safari/537.36");
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        let req = b.body(body.map(|v| Body::from(v.to_string())).unwrap_or_else(Body::empty)).unwrap();
        let resp = router(self.state.clone()).oneshot(req).await.unwrap();
        let st = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (st, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn register(&self, phone: &str) -> String {
        let (s, v) = self.call("POST", "/register", None, Some(json!({ "phone": phone, "password": "correct horse 42" }))).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["token"].as_str().unwrap().to_string()
    }

    async fn credit(&self, token: &str, kobo: i64) {
        let acct: Uuid = sqlx::query_scalar("SELECT account_id FROM member_sessions WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT 1")
            .bind(self.tenant)
            .fetch_one(&self.admin)
            .await
            .unwrap();
        let _ = token;
        sqlx::query("INSERT INTO wallet_ledger (tenant_id, account_id, kind, amount_kobo, note) VALUES ($1, $2, 'adjustment', $3, 'test')")
            .bind(self.tenant)
            .bind(acct)
            .bind(kobo)
            .execute(&self.admin)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn register_login_and_me() {
    let Some(w) = world().await else { return };
    let token = w.register("08011110001").await;
    let (s, me) = w.call("GET", "/me", Some(&token), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(me["phone"], "+2348011110001");
    assert_eq!(me["balance_kobo"], 0);
    let (s, _) = w.call("POST", "/register", None, Some(json!({ "phone": "08011110001", "password": "another pass 99" }))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "no duplicate accounts");
    let (s, v) = w.call("POST", "/login", None, Some(json!({ "phone": "+2348011110001", "password": "correct horse 42" }))).await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["token"].as_str().unwrap().starts_with("mt_"));
    // Only a hash is stored.
    let leaked: i64 = sqlx::query_scalar("SELECT count(*) FROM member_sessions WHERE token_hash = $1").bind(&token).fetch_one(&w.admin).await.unwrap();
    assert_eq!(leaked, 0);
    // Logout revokes.
    w.call("POST", "/logout", Some(&token), None).await;
    assert_eq!(w.call("GET", "/me", Some(&token), None).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn weak_passwords_and_bad_tokens_are_refused() {
    let Some(w) = world().await else { return };
    let (s, _) = w.call("POST", "/register", None, Some(json!({ "phone": "08011110002", "password": "short" }))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(w.call("GET", "/me", None, None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(w.call("GET", "/me", Some("mt_notarealtoken"), None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(w.call("GET", "/me", Some("at_adminlooking"), None).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn five_wrong_passwords_lock_the_account_even_for_the_right_one() {
    let Some(w) = world().await else { return };
    w.register("08011110003").await;
    for _ in 0..5 {
        let (s, _) = w.call("POST", "/login", None, Some(json!({ "phone": "08011110003", "password": "wrong wrong 1" }))).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }
    let (s, _) = w.call("POST", "/login", None, Some(json!({ "phone": "08011110003", "password": "correct horse 42" }))).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "locked");
    // Unknown number looks the same as a wrong password.
    let (s, _) = w.call("POST", "/login", None, Some(json!({ "phone": "08099990000", "password": "whatever 123" }))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn sms_code_signs_in_once_and_wrong_codes_burn_attempts() {
    let Some(w) = world().await else { return };
    let (s, _) = w.call("POST", "/otp/request", None, Some(json!({ "phone": "08011110004" }))).await;
    assert_eq!(s, StatusCode::OK);
    let text = w.sms.0.lock().unwrap().last().unwrap().1.clone();
    let code: String = text
        .split_whitespace()
        .find(|t| t.trim_end_matches('.').len() == 6 && t.trim_end_matches('.').chars().all(|c| c.is_ascii_digit()))
        .unwrap()
        .trim_end_matches('.')
        .to_string();
    let wrong = if code == "000000" { "111111" } else { "000000" };
    let (s, _) = w.call("POST", "/otp/verify", None, Some(json!({ "phone": "08011110004", "code": wrong }))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, v) = w.call("POST", "/otp/verify", None, Some(json!({ "phone": "08011110004", "code": code }))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, _) = w.call("POST", "/otp/verify", None, Some(json!({ "phone": "08011110004", "code": code }))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "a code works once");
}

#[tokio::test]
async fn sms_code_is_dead_after_five_wrong_tries() {
    let Some(w) = world().await else { return };
    w.call("POST", "/otp/request", None, Some(json!({ "phone": "08011110005" }))).await;
    let text = w.sms.0.lock().unwrap().last().unwrap().1.clone();
    let code: String = text
        .split_whitespace()
        .find(|t| t.trim_end_matches('.').len() == 6 && t.trim_end_matches('.').chars().all(|c| c.is_ascii_digit()))
        .unwrap()
        .trim_end_matches('.')
        .to_string();
    let wrong = if code == "000000" { "111111" } else { "000000" };
    for _ in 0..5 {
        w.call("POST", "/otp/verify", None, Some(json!({ "phone": "08011110005", "code": wrong }))).await;
    }
    let (s, _) = w.call("POST", "/otp/verify", None, Some(json!({ "phone": "08011110005", "code": code }))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "even the right code is refused after 5 misses");
}

#[tokio::test]
async fn topup_credits_once_and_buying_debits_and_grants() {
    let Some(w) = world().await else { return };
    let token = w.register("08011110006").await;
    let (s, v) = w.call("POST", "/wallet/topup", Some(&token), Some(json!({ "site": "site-1", "amount_kobo": 50000 }))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let reference = v["reference"].as_str().unwrap().to_string();
    let tx = VerifiedTransaction {
        id: "t1".into(),
        reference: reference.clone(),
        status: "success".into(),
        amount_kobo: 50000,
        currency: "NGN".into(),
        channel: None,
        fees_kobo: 0,
        raw: json!({}),
    };
    assert!(matches!(fulfil_payment(&w.state.pool, w.tenant, &tx).await.unwrap(), Fulfilment::WalletCredited { amount_kobo: 50000 }));
    assert!(matches!(fulfil_payment(&w.state.pool, w.tenant, &tx).await.unwrap(), Fulfilment::AlreadyProcessed));
    let (_, wal) = w.call("GET", "/wallet", Some(&token), None).await;
    assert_eq!(wal["balance_kobo"], 50000, "credited exactly once");

    let (s, v) = w.call("POST", "/wallet/buy", Some(&token), Some(json!({ "site": "site-1", "plan_id": w.plan_id }))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["balance_kobo"], 40000);
    let (_, me) = w.call("GET", "/me", Some(&token), None).await;
    assert_eq!(me["entitlements"].as_array().unwrap().len(), 1);
    assert_eq!(me["balance_kobo"], 40000);
}

#[tokio::test]
async fn topup_amount_limits_and_overspend_are_refused() {
    let Some(w) = world().await else { return };
    let token = w.register("08011110007").await;
    for bad in [0, 5000, 99_999_999_999i64, -100] {
        let (s, _) = w.call("POST", "/wallet/topup", Some(&token), Some(json!({ "site": "site-1", "amount_kobo": bad }))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "amount {bad}");
    }
    let (s, _) = w.call("POST", "/wallet/buy", Some(&token), Some(json!({ "site": "site-1", "plan_id": w.plan_id }))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "no balance, no purchase");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM entitlements WHERE tenant_id = $1").bind(w.tenant).fetch_one(&w.admin).await.unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn concurrent_purchases_cannot_spend_the_same_money_twice() {
    let Some(w) = world().await else { return };
    let token = w.register("08011110008").await;
    w.credit(&token, 10000).await; // exactly one plan
    let mut handles = Vec::new();
    for _ in 0..6 {
        let (st, t, plan) = (w.state.clone(), token.clone(), w.plan_id);
        let slug = w.slug.clone();
        handles.push(tokio::spawn(async move {
            let req = Request::post(format!("/v1/t/{slug}/members/wallet/buy"))
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {t}"))
                .body(Body::from(json!({ "site": "site-1", "plan_id": plan }).to_string()))
                .unwrap();
            router(st).oneshot(req).await.unwrap().status()
        }));
    }
    let mut ok = 0;
    for h in handles {
        if h.await.unwrap() == StatusCode::OK {
            ok += 1;
        }
    }
    assert_eq!(ok, 1, "one balance buys one plan");
    let bal: i64 = sqlx::query_scalar("SELECT COALESCE(sum(amount_kobo),0)::bigint FROM wallet_ledger WHERE tenant_id = $1")
        .bind(w.tenant)
        .fetch_one(&w.admin)
        .await
        .unwrap();
    assert_eq!(bal, 0);
}

#[tokio::test]
async fn a_guest_code_can_be_claimed_once() {
    let Some(w) = world().await else { return };
    let token = w.register("08011110009").await;
    let code_id: Uuid = sqlx::query_scalar("INSERT INTO access_codes (tenant_id, code, phone) VALUES ($1, 'KKKK2222', '+2348000000000') RETURNING id")
        .bind(w.tenant)
        .fetch_one(&w.admin)
        .await
        .unwrap();
    let plan = serde_json::to_value(sample_plan()).unwrap();
    sqlx::query("INSERT INTO entitlements (tenant_id, plan_id, plan_snapshot, access_code_id, status, expires_at, remaining_active_seconds) VALUES ($1, $2, $3, $4, 'pending', now() + interval '30 days', 3600)")
        .bind(w.tenant).bind(w.plan_id).bind(plan).bind(code_id).execute(&w.admin).await.unwrap();
    let (s, v) = w.call("POST", "/claim", Some(&token), Some(json!({ "code": "kkkk 2222" }))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["moved"], 1);
    let (_, me) = w.call("GET", "/me", Some(&token), None).await;
    assert_eq!(me["entitlements"].as_array().unwrap().len(), 1);
    let other = w.register("08011110010").await;
    let (s, _) = w.call("POST", "/claim", Some(&other), Some(json!({ "code": "KKKK2222" }))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "already claimed");
}

fn sample_plan() -> entitlement::Plan {
    entitlement::Plan {
        kind: entitlement::PlanKind::Time,
        active_seconds: Some(3600),
        data_bytes: None,
        valid_seconds: 86_400,
        activate_on_first_use: true,
        unused_expiry_seconds: 90 * 86_400,
        device_limit: 1,
        swap_cooldown_secs: 86_400,
        speed: entitlement::SpeedProfile { down_kbps: 5000, up_kbps: 2000, burst: None, priority: 8 },
        active_threshold_bytes: 20_480,
        idle_timeout_secs: 300,
        fair_use: None,
    }
}

#[tokio::test]
async fn a_member_gets_a_ticket_and_a_second_device_must_swap_with_cooldown() {
    let Some(w) = world().await else { return };
    let token = w.register("08011110011").await;
    w.credit(&token, 20000).await;
    let (s, _) = w.call("POST", "/wallet/buy", Some(&token), Some(json!({ "site": "site-1", "plan_id": w.plan_id }))).await;
    assert_eq!(s, StatusCode::OK);

    let (s, _) = w.call("POST", "/connect", Some(&token), Some(json!({ "site": "site-1", "mac": "A4:83:E7:12:34:56" }))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "terms must be accepted");
    let (s, t) = w.call("POST", "/connect", Some(&token), Some(json!({ "site": "site-1", "mac": "A4:83:E7:12:34:56", "accept_terms": true }))).await;
    assert_eq!(s, StatusCode::OK, "{t}");
    assert!(t["username"].as_str().unwrap().starts_with("tk") && t["password"].as_str().unwrap().starts_with("tp_"));

    // Pretend the first device holds the single slot (the router link does this on authorize).
    let dev1: Uuid =
        sqlx::query_scalar("SELECT id FROM devices WHERE tenant_id = $1 AND mac = 'a4:83:e7:12:34:56'").bind(w.tenant).fetch_one(&w.admin).await.unwrap();
    let ent: Uuid = sqlx::query_scalar("SELECT id FROM entitlements WHERE tenant_id = $1").bind(w.tenant).fetch_one(&w.admin).await.unwrap();
    sqlx::query("INSERT INTO device_bindings (tenant_id, entitlement_id, device_id) VALUES ($1, $2, $3)")
        .bind(w.tenant)
        .bind(ent)
        .bind(dev1)
        .execute(&w.admin)
        .await
        .unwrap();
    let (_, d) = w.call("GET", "/devices", Some(&token), None).await;
    assert_eq!(d["devices"].as_array().unwrap().len(), 1);

    // Swap to a device that never signed in: refused.
    let swap = |mac: &str| json!({ "entitlement_id": ent, "old_device_id": dev1, "new_mac": mac });
    let (s, _) = w.call("POST", "/devices/swap", Some(&token), Some(swap("3C:22:FB:00:00:01"))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // New device signs in, then the swap works.
    w.call("POST", "/connect", Some(&token), Some(json!({ "site": "site-1", "mac": "3C:22:FB:00:00:01", "accept_terms": true }))).await;
    let (s, v) = w.call("POST", "/devices/swap", Some(&token), Some(swap("3C:22:FB:00:00:01"))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // A second swap straight away hits the 24 h cooldown.
    let dev2: Uuid =
        sqlx::query_scalar("SELECT id FROM devices WHERE tenant_id = $1 AND mac = '3c:22:fb:00:00:01'").bind(w.tenant).fetch_one(&w.admin).await.unwrap();
    w.call("POST", "/connect", Some(&token), Some(json!({ "site": "site-1", "mac": "A4:83:E7:12:34:56", "accept_terms": true }))).await;
    let (s, _) =
        w.call("POST", "/devices/swap", Some(&token), Some(json!({ "entitlement_id": ent, "old_device_id": dev2, "new_mac": "A4:83:E7:12:34:56" }))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "cooldown");
}

#[tokio::test]
async fn one_member_cannot_touch_anothers_plan_or_wallet() {
    let Some(w) = world().await else { return };
    let a = w.register("08011110012").await;
    w.credit(&a, 10000).await;
    let (_, v) = w.call("POST", "/wallet/buy", Some(&a), Some(json!({ "site": "site-1", "plan_id": w.plan_id }))).await;
    assert!(v["entitlement_id"].is_string());
    let b = w.register("08011110013").await;
    let ent: Uuid = sqlx::query_scalar("SELECT id FROM entitlements WHERE tenant_id = $1").bind(w.tenant).fetch_one(&w.admin).await.unwrap();
    let (s, _) = w
        .call("POST", "/devices/swap", Some(&b), Some(json!({ "entitlement_id": ent, "old_device_id": Uuid::new_v4(), "new_mac": "3C:22:FB:00:00:09" })))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (_, me) = w.call("GET", "/me", Some(&b), None).await;
    assert_eq!(me["entitlements"].as_array().unwrap().len(), 0);
    assert_eq!(me["balance_kobo"], 0);
}
