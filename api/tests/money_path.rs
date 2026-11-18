//! End-to-end tests of the payment path against a REAL PostgreSQL database,
//! driving the real HTTP router with a fake Paystack.
//!
//! Needs two environment variables (see `run_integration.sh`, which sets them up):
//! * `DATABASE_URL`        connects as `app_test`, a member of `platform_app`
//!   (not a superuser, so row-level security applies)
//! * `ADMIN_DATABASE_URL`  connects as the owner, used only to create test fixtures
//!
//! Without them every test prints a notice and passes, so `cargo test` still
//! works on a machine with no database.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use platform_api::error::{AppError, Result};
use platform_api::http::router;
use platform_api::paystack::{sign, Gateway, InitializeRequest, InitializedTransaction, VerifiedTransaction};
use platform_api::secrets::MasterKey;
use platform_api::{AppState, Config};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use tower::ServiceExt;
use uuid::Uuid;

const MASTER: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const SECRET: &str = "sk_test_integration_secret";

/// A Paystack stand-in. Tests tell it what `verify` should answer.
#[derive(Default)]
struct FakeGateway {
    answers: Mutex<HashMap<String, VerifiedTransaction>>,
    fail_initialize: Mutex<bool>,
}

impl FakeGateway {
    fn will_confirm(&self, reference: &str, amount_kobo: i64, currency: &str, status: &str) {
        self.answers.lock().unwrap().insert(
            reference.to_string(),
            VerifiedTransaction {
                id: format!("txn_{}", Uuid::new_v4().simple()),
                reference: reference.to_string(),
                status: status.to_string(),
                amount_kobo,
                currency: currency.to_string(),
                channel: Some("bank_transfer".into()),
                fees_kobo: 750,
                raw: json!({ "status": true, "data": { "reference": reference, "amount": amount_kobo } }),
            },
        );
    }
}

#[async_trait]
impl Gateway for FakeGateway {
    async fn initialize(&self, req: InitializeRequest<'_>) -> Result<InitializedTransaction> {
        if *self.fail_initialize.lock().unwrap() {
            return Err(AppError::Gateway("simulated outage".into()));
        }
        assert_eq!(req.secret, SECRET, "the tenant's own decrypted key must be used");
        Ok(InitializedTransaction { authorization_url: format!("https://checkout.paystack.test/{}", req.reference), reference: req.reference.to_string() })
    }

    async fn verify(&self, secret: &str, reference: &str) -> Result<VerifiedTransaction> {
        assert_eq!(secret, SECRET);
        self.answers.lock().unwrap().get(reference).cloned().ok_or_else(|| AppError::Gateway("unknown transaction".into()))
    }
}

struct World {
    app: PgPool,
    admin: PgPool,
    gateway: Arc<FakeGateway>,
    state: AppState,
    slug: String,
    tenant: Uuid,
    plan_id: Uuid,
}

/// Build a fresh tenant (own slug, site, plan, encrypted key) so tests never share rows.
async fn world() -> Option<World> {
    let (Ok(app_url), Ok(admin_url)) = (std::env::var("DATABASE_URL"), std::env::var("ADMIN_DATABASE_URL")) else {
        eprintln!("NOTICE: DATABASE_URL / ADMIN_DATABASE_URL not set; skipping integration test");
        return None;
    };
    let app = PgPoolOptions::new().max_connections(5).connect(&app_url).await.unwrap();
    let admin = PgPoolOptions::new().max_connections(2).connect(&admin_url).await.unwrap();

    let slug = format!("t-{}", &Uuid::new_v4().simple().to_string()[..10]);
    let tenant: Uuid = sqlx::query_scalar("INSERT INTO tenants (name, slug) VALUES ($1, $2) RETURNING id")
        .bind(format!("Test {slug}"))
        .bind(&slug)
        .fetch_one(&admin)
        .await
        .unwrap();
    let key = MasterKey::from_hex(MASTER).unwrap();
    sqlx::query("INSERT INTO tenant_secrets (tenant_id, paystack_secret_enc) VALUES ($1, $2)")
        .bind(tenant)
        .bind(key.encrypt(SECRET).unwrap())
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query("INSERT INTO sites (tenant_id, name, slug) VALUES ($1, 'Site 1', 'site-1')").bind(tenant).execute(&admin).await.unwrap();
    let profile: Uuid = sqlx::query_scalar("INSERT INTO speed_profiles (tenant_id, name, down_kbps, up_kbps) VALUES ($1, 'Standard', 5000, 2000) RETURNING id")
        .bind(tenant)
        .fetch_one(&admin)
        .await
        .unwrap();
    let plan_id: Uuid = sqlx::query_scalar(
        "INSERT INTO plans (tenant_id, name, kind, price_kobo, active_seconds, valid_seconds, speed_profile_id)
         VALUES ($1, 'Daily', 'time', 50000, 86400, 86400, $2) RETURNING id",
    )
    .bind(tenant)
    .bind(profile)
    .fetch_one(&admin)
    .await
    .unwrap();

    let gateway = Arc::new(FakeGateway::default());
    let state = AppState {
        pool: app.clone(),
        gateway: gateway.clone(),
        master_key: key,
        config: Config { public_base_url: "https://wifi.test".into(), fallback_email_domain: "guest.test".into(), trust_proxy: false, require_2fa: false },
        limiter: Arc::new(platform_api::auth::RateLimiter::new()),
        sms: Arc::new(platform_api::sms::LogSms),
        mailer: Arc::new(platform_api::mail::MemoryMailer::default()),
    };
    Some(World { app, admin, gateway, state, slug, tenant, plan_id })
}

async fn call(state: &AppState, req: Request<Body>) -> (StatusCode, Value) {
    let resp = router(state.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn json_post(uri: &str, body: &Value) -> Request<Body> {
    Request::post(uri).header("content-type", "application/json").body(Body::from(body.to_string())).unwrap()
}

async fn place_order(w: &World) -> String {
    let (status, v) =
        call(&w.state, json_post(&format!("/v1/t/{}/orders", w.slug), &json!({ "site": "site-1", "plan_id": w.plan_id, "phone": "0801 234 5678" }))).await;
    assert_eq!(status, StatusCode::CREATED, "order response: {v}");
    assert_eq!(v["amount_kobo"], 50000, "price comes from the plan, not the browser");
    v["reference"].as_str().unwrap().to_string()
}

fn webhook(w: &World, reference: &str, secret_for_signature: &str) -> Request<Body> {
    let body = json!({ "event": "charge.success", "data": { "reference": reference } }).to_string();
    Request::post(format!("/v1/t/{}/webhooks/paystack", w.slug))
        .header("x-paystack-signature", sign(secret_for_signature, body.as_bytes()))
        .body(Body::from(body))
        .unwrap()
}

async fn count(w: &World, sql: &str, bind: Uuid) -> i64 {
    sqlx::query_scalar(sql).bind(bind).fetch_one(&w.admin).await.unwrap()
}

#[tokio::test]
async fn a_paid_guest_order_becomes_an_access_code_and_an_entitlement() {
    let Some(w) = world().await else { return };

    // Portal lists the plan.
    let (status, plans) = call(&w.state, Request::get(format!("/v1/t/{}/sites/site-1/plans", w.slug)).body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(plans[0]["name"], "Daily");
    assert_eq!(plans[0]["price_kobo"], 50000);

    let reference = place_order(&w).await;
    let (_, before) = call(&w.state, Request::get(format!("/v1/t/{}/orders/{reference}", w.slug)).body(Body::empty()).unwrap()).await;
    assert_eq!(before["status"], "pending");
    assert!(before["access_code"].is_null(), "no code before payment");

    w.gateway.will_confirm(&reference, 50000, "NGN", "success");
    let (status, v) = call(&w.state, webhook(&w, &reference, SECRET)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["result"], "provisioned");
    let code = v["access_code"].as_str().expect("guest gets a code");
    assert_eq!(code.len(), 8);

    let (_, after) = call(&w.state, Request::get(format!("/v1/t/{}/orders/{reference}", w.slug)).body(Body::empty()).unwrap()).await;
    assert_eq!(after["status"], "provisioned");
    assert_eq!(after["access_code"], code);

    // What was stored.
    let row = sqlx::query(
        "SELECT status::text AS status, remaining_active_seconds, remaining_bytes, plan_snapshot,
                (expires_at > now() + interval '80 days') AS unused_window_is_long
           FROM entitlements WHERE tenant_id = $1",
    )
    .bind(w.tenant)
    .fetch_one(&w.admin)
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("status"), "pending", "starts on first connection");
    assert_eq!(row.get::<Option<i32>, _>("remaining_active_seconds"), Some(86400));
    assert_eq!(row.get::<Option<i64>, _>("remaining_bytes"), None);
    assert_eq!(row.get::<Value, _>("plan_snapshot")["valid_seconds"], 86400);
    assert!(row.get::<bool, _>("unused_window_is_long"), "unused purchase keeps its 90-day expiry");
    assert_eq!(count(&w, "SELECT count(*) FROM payments WHERE tenant_id = $1 AND verified", w.tenant).await, 1);

    // The audit trail is intact.
    let broken: i64 = sqlx::query_scalar("SELECT count(*) FROM verify_audit_chain($1)").bind(w.tenant).fetch_one(&w.admin).await.unwrap();
    assert_eq!(broken, 0);
    assert!(count(&w, "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND action = 'payment.confirmed'", w.tenant).await == 1);
}

#[tokio::test]
async fn a_repeated_webhook_grants_access_only_once() {
    let Some(w) = world().await else { return };
    let reference = place_order(&w).await;
    w.gateway.will_confirm(&reference, 50000, "NGN", "success");

    let (_, first) = call(&w.state, webhook(&w, &reference, SECRET)).await;
    assert_eq!(first["result"], "provisioned");
    for _ in 0..3 {
        let (status, again) = call(&w.state, webhook(&w, &reference, SECRET)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(again["result"], "already_processed");
    }
    assert_eq!(count(&w, "SELECT count(*) FROM entitlements WHERE tenant_id = $1", w.tenant).await, 1);
    assert_eq!(count(&w, "SELECT count(*) FROM payments WHERE tenant_id = $1", w.tenant).await, 1);
    assert_eq!(count(&w, "SELECT count(*) FROM access_codes WHERE tenant_id = $1", w.tenant).await, 1);
}

#[tokio::test]
async fn concurrent_webhooks_for_one_order_still_grant_once() {
    let Some(w) = world().await else { return };
    let reference = place_order(&w).await;
    w.gateway.will_confirm(&reference, 50000, "NGN", "success");

    let mut tasks = Vec::new();
    for _ in 0..8 {
        let st = w.state.clone();
        let req = webhook(&w, &reference, SECRET);
        tasks.push(tokio::spawn(async move { call(&st, req).await }));
    }
    let mut provisioned = 0;
    for t in tasks {
        let (status, v) = t.await.unwrap();
        assert_eq!(status, StatusCode::OK, "{v}");
        if v["result"] == "provisioned" {
            provisioned += 1;
        }
    }
    assert_eq!(provisioned, 1, "exactly one webhook wins the race");
    assert_eq!(count(&w, "SELECT count(*) FROM entitlements WHERE tenant_id = $1", w.tenant).await, 1);
}

#[tokio::test]
async fn a_forged_webhook_is_rejected_and_logged() {
    let Some(w) = world().await else { return };
    let reference = place_order(&w).await;
    w.gateway.will_confirm(&reference, 50000, "NGN", "success");

    let (status, _) = call(&w.state, webhook(&w, &reference, "sk_test_attacker_guess")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let no_header = Request::post(format!("/v1/t/{}/webhooks/paystack", w.slug))
        .body(Body::from(json!({ "event": "charge.success", "data": { "reference": reference } }).to_string()))
        .unwrap();
    assert_eq!(call(&w.state, no_header).await.0, StatusCode::UNAUTHORIZED);

    assert_eq!(count(&w, "SELECT count(*) FROM entitlements WHERE tenant_id = $1", w.tenant).await, 0);
    assert_eq!(count(&w, "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND action = 'payment.webhook_rejected'", w.tenant).await, 2);
}

#[tokio::test]
async fn the_wrong_amount_is_held_for_refund_not_provisioned() {
    let Some(w) = world().await else { return };
    let reference = place_order(&w).await;
    w.gateway.will_confirm(&reference, 10000, "NGN", "success"); // paid 100, owed 500

    let (status, v) = call(&w.state, webhook(&w, &reference, SECRET)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["result"], "amount_mismatch");
    assert_eq!(count(&w, "SELECT count(*) FROM entitlements WHERE tenant_id = $1", w.tenant).await, 0);
    let st: String = sqlx::query_scalar("SELECT status::text FROM orders WHERE reference = $1").bind(&reference).fetch_one(&w.admin).await.unwrap();
    assert_eq!(st, "refund_flagged");
    assert_eq!(count(&w, "SELECT count(*) FROM payments WHERE tenant_id = $1 AND NOT verified", w.tenant).await, 1);
    assert_eq!(count(&w, "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND action = 'payment.amount_mismatch'", w.tenant).await, 1);
}

#[tokio::test]
async fn a_failed_or_abandoned_transaction_grants_nothing() {
    let Some(w) = world().await else { return };
    let reference = place_order(&w).await;
    w.gateway.will_confirm(&reference, 50000, "NGN", "abandoned");
    let (_, v) = call(&w.state, webhook(&w, &reference, SECRET)).await;
    assert_eq!(v["result"], "not_successful");
    assert_eq!(count(&w, "SELECT count(*) FROM entitlements WHERE tenant_id = $1", w.tenant).await, 0);
}

#[tokio::test]
async fn a_gateway_outage_marks_the_order_failed_and_tells_the_customer_to_retry() {
    let Some(w) = world().await else { return };
    *w.gateway.fail_initialize.lock().unwrap() = true;
    let (status, v) =
        call(&w.state, json_post(&format!("/v1/t/{}/orders", w.slug), &json!({ "site": "site-1", "plan_id": w.plan_id, "phone": "08012345678" }))).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(!v["error"].as_str().unwrap().contains("simulated"), "internal detail must not leak");
    assert_eq!(count(&w, "SELECT count(*) FROM orders WHERE tenant_id = $1 AND status = 'failed'", w.tenant).await, 1);
}

#[tokio::test]
async fn bad_input_is_refused() {
    let Some(w) = world().await else { return };
    let uri = format!("/v1/t/{}/orders", w.slug);
    let (s, _) = call(&w.state, json_post(&uri, &json!({ "site": "site-1", "plan_id": w.plan_id, "phone": "123" }))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = call(&w.state, json_post(&uri, &json!({ "site": "nope", "plan_id": w.plan_id, "phone": "08012345678" }))).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(&w.state, json_post(&uri, &json!({ "site": "site-1", "plan_id": Uuid::new_v4(), "phone": "08012345678" }))).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(&w.state, json_post("/v1/t/no-such-tenant/orders", &json!({ "site": "site-1", "plan_id": w.plan_id, "phone": "08012345678" }))).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // an oversized body is refused before it is read
    let big = Request::post(&uri).header("content-type", "application/json").body(Body::from(vec![b'a'; 200_000])).unwrap();
    assert_eq!(call(&w.state, big).await.0, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn one_tenant_cannot_see_or_settle_anothers_orders() {
    let Some(a) = world().await else { return };
    let Some(b) = world().await else { return };
    let reference = place_order(&a).await;

    // Tenant B asks about A's order, under B's own URL.
    let (s, _) = call(&b.state, Request::get(format!("/v1/t/{}/orders/{reference}", b.slug)).body(Body::empty()).unwrap()).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // A webhook signed with B's key for A's reference finds nothing to settle.
    b.gateway.will_confirm(&reference, 50000, "NGN", "success");
    let (s, v) = call(&b.state, webhook(&b, &reference, SECRET)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["result"], "unknown_reference");
    assert_eq!(count(&a, "SELECT count(*) FROM entitlements WHERE tenant_id = $1", a.tenant).await, 0);

    // And the app role really is subject to row-level security: with no tenant set it sees nothing.
    let seen: i64 = sqlx::query_scalar("SELECT count(*) FROM orders").fetch_one(&a.app).await.unwrap();
    assert_eq!(seen, 0, "no tenant context means no rows");
}
