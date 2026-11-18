//! Refunds against a real PostgreSQL with row-level security on. See `run_integration.sh`.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use platform_api::auth::RateLimiter;
use platform_api::error::{AppError, Result};
use platform_api::fulfil::fulfil_payment;
use platform_api::http::router;
use platform_api::paystack::{Gateway, InitializeRequest, InitializedTransaction, RefundResult, VerifiedTransaction};
use platform_api::secrets::MasterKey;
use platform_api::{AppState, Config};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

#[derive(Default)]
struct Bank {
    refunds: Mutex<Vec<(String, i64)>>,
    fail: Mutex<bool>,
}
struct FakeGateway(Arc<Bank>);
#[async_trait]
impl Gateway for FakeGateway {
    async fn initialize(&self, r: InitializeRequest<'_>) -> Result<InitializedTransaction> {
        Ok(InitializedTransaction { authorization_url: format!("https://checkout.test/{}", r.reference), reference: r.reference.to_string() })
    }
    async fn verify(&self, _: &str, _: &str) -> Result<VerifiedTransaction> {
        unreachable!()
    }
    async fn refund(&self, _: &str, reference: &str, amount_kobo: i64, _: &str) -> Result<RefundResult> {
        if *self.0.fail.lock().unwrap() {
            return Err(AppError::Gateway("paystack said no".into()));
        }
        self.0.refunds.lock().unwrap().push((reference.to_string(), amount_kobo));
        Ok(RefundResult { provider_ref: "rf_1".into(), status: "pending".into() })
    }
}

struct World {
    admin: PgPool,
    state: AppState,
    bank: Arc<Bank>,
    slug: String,
    tenant: Uuid,
    plan_id: Uuid,
}

async fn world() -> Option<World> {
    let (Ok(app_url), Ok(admin_url)) = (std::env::var("DATABASE_URL"), std::env::var("ADMIN_DATABASE_URL")) else {
        eprintln!("NOTICE: DATABASE_URL / ADMIN_DATABASE_URL not set; skipping integration test");
        return None;
    };
    let app = PgPoolOptions::new().max_connections(8).connect(&app_url).await.unwrap();
    let admin = PgPoolOptions::new().max_connections(2).connect(&admin_url).await.unwrap();
    let slug = format!("r-{}", &Uuid::new_v4().simple().to_string()[..10]);
    let tenant: Uuid = sqlx::query_scalar("INSERT INTO tenants (name, slug) VALUES ($1, $1) RETURNING id").bind(&slug).fetch_one(&admin).await.unwrap();
    sqlx::query("INSERT INTO sites (tenant_id, name, slug) VALUES ($1, 'Site 1', 'site-1')").bind(tenant).execute(&admin).await.unwrap();
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
    let bank = Arc::new(Bank::default());
    let state = AppState {
        pool: app,
        gateway: Arc::new(FakeGateway(bank.clone())),
        master_key: key,
        config: Config { public_base_url: "https://wifi.test".into(), fallback_email_domain: "guest.test".into(), trust_proxy: false, require_2fa: false },
        limiter: Arc::new(RateLimiter::new()),
        sms: Arc::new(platform_api::sms::LogSms),
        mailer: Arc::new(platform_api::mail::MemoryMailer::default()),
    };
    Some(World { admin, state, bank, slug, tenant, plan_id })
}

impl World {
    async fn call(&self, method: &str, path: &str, token: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
        let mut b = Request::builder().method(method).uri(format!("/v1/t/{}{}", self.slug, path)).header("content-type", "application/json");
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        let resp = router(self.state.clone()).oneshot(b.body(body.map(|v| Body::from(v.to_string())).unwrap_or_else(Body::empty)).unwrap()).await.unwrap();
        let st = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (st, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn staff(&self, email: &str, role: &str) -> String {
        platform_api::admin::create_admin(&self.state, &self.slug, email, "correct horse 42", role).await.unwrap();
        let (s, v) = self.call("POST", "/admin/login", None, Some(json!({ "email": email, "password": "correct horse 42" }))).await;
        assert_eq!(s, StatusCode::OK);
        v["token"].as_str().unwrap().to_string()
    }

    /// A guest buys the plan and Paystack confirms: returns (order reference, order id).
    async fn guest_order(&self, paid_kobo: i64) -> (String, Uuid) {
        let (s, v) = self.call("POST", "/orders", None, Some(json!({ "site": "site-1", "plan_id": self.plan_id, "phone": "08031112222" }))).await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        let reference = v["reference"].as_str().unwrap().to_string();
        let tx = VerifiedTransaction {
            id: format!("t-{reference}"),
            reference: reference.clone(),
            status: "success".into(),
            amount_kobo: paid_kobo,
            currency: "NGN".into(),
            channel: None,
            fees_kobo: 0,
            raw: json!({}),
        };
        fulfil_payment(&self.state.pool, self.tenant, &tx).await.unwrap();
        let id: Uuid = sqlx::query_scalar("SELECT id FROM orders WHERE reference = $1").bind(&reference).fetch_one(&self.admin).await.unwrap();
        (reference, id)
    }

    async fn member(&self, phone: &str) -> (String, Uuid) {
        let (_, v) = self.call("POST", "/members/register", None, Some(json!({ "phone": phone, "password": "correct horse 42" }))).await;
        (v["token"].as_str().unwrap().to_string(), v["account_id"].as_str().unwrap().parse().unwrap())
    }

    async fn status_of(&self, order: Uuid) -> String {
        sqlx::query_scalar("SELECT status::text FROM orders WHERE id = $1").bind(order).fetch_one(&self.admin).await.unwrap()
    }
    async fn balance(&self, acct: Uuid) -> i64 {
        sqlx::query_scalar("SELECT COALESCE(sum(amount_kobo),0)::bigint FROM wallet_ledger WHERE account_id = $1")
            .bind(acct)
            .fetch_one(&self.admin)
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn customer_asks_owner_refunds_once_and_the_plan_stops_working() {
    let Some(w) = world().await else { return };
    let (reference, order) = w.guest_order(10000).await;
    let owner = w.staff("o@x.test", "system_admin").await;

    let (s, v) = w.call("POST", &format!("/orders/{reference}/refund"), None, Some(json!({ "reason": "Router never let me in" }))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(w.call("POST", &format!("/orders/{reference}/refund"), None, Some(json!({}))).await.0, StatusCode::BAD_REQUEST, "no second request");
    assert_eq!(w.call("GET", &format!("/orders/{reference}/refund"), None, None).await.1["status"], "requested");
    let (_, queue) = w.call("GET", "/admin/refunds?status=requested", Some(&owner), None).await;
    assert_eq!(queue[0]["reason"], "Router never let me in");

    let (s, v) = w.call("POST", &format!("/admin/orders/{order}/refund"), Some(&owner), Some(json!({ "note": "approved" }))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["method"], "paystack");
    assert_eq!(*w.bank.refunds.lock().unwrap(), vec![(reference.clone(), 10000)]);
    assert_eq!(w.status_of(order).await, "refunded");
    let live: i64 =
        sqlx::query_scalar("SELECT count(*) FROM entitlements WHERE order_id = $1 AND status <> 'cancelled'").bind(order).fetch_one(&w.admin).await.unwrap();
    assert_eq!(live, 0, "the plan is cancelled");
    assert_eq!(w.call("GET", &format!("/orders/{reference}/refund"), None, None).await.1["status"], "processed");

    // Never twice.
    assert_eq!(w.call("POST", &format!("/admin/orders/{order}/refund"), Some(&owner), Some(json!({}))).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(w.call("POST", &format!("/orders/{reference}/refund"), None, Some(json!({}))).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(w.bank.refunds.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn only_owners_send_money_but_operators_can_decline() {
    let Some(w) = world().await else { return };
    let (reference, order) = w.guest_order(10000).await;
    let operator = w.staff("op@x.test", "admin").await;
    let viewer = w.staff("v@x.test", "customer_rep").await;
    w.call("POST", &format!("/orders/{reference}/refund"), None, Some(json!({ "reason": "changed my mind" }))).await;
    assert_eq!(w.call("POST", &format!("/admin/orders/{order}/refund"), Some(&operator), Some(json!({}))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(w.call("POST", &format!("/admin/orders/{order}/refund"), Some(&viewer), Some(json!({}))).await.0, StatusCode::FORBIDDEN);
    assert!(w.bank.refunds.lock().unwrap().is_empty());
    let (_, q) = w.call("GET", "/admin/refunds?status=requested", Some(&operator), None).await;
    let rid = q[0]["id"].as_str().unwrap().to_string();
    assert_eq!(w.call("POST", &format!("/admin/refunds/{rid}/decline"), Some(&viewer), Some(json!({ "note": "x" }))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(w.call("POST", &format!("/admin/refunds/{rid}/decline"), Some(&operator), Some(json!({ "note": "plan was used" }))).await.0, StatusCode::OK);
    assert_eq!(w.call("GET", &format!("/orders/{reference}/refund"), None, None).await.1["status"], "declined");
    assert_eq!(w.status_of(order).await, "provisioned", "declining changes nothing");
}

#[tokio::test]
async fn when_paystack_refuses_nothing_changes_and_a_retry_works() {
    let Some(w) = world().await else { return };
    let (_, order) = w.guest_order(10000).await;
    let owner = w.staff("o2@x.test", "system_admin").await;
    *w.bank.fail.lock().unwrap() = true;
    let (s, _) = w.call("POST", &format!("/admin/orders/{order}/refund"), Some(&owner), Some(json!({}))).await;
    assert_eq!(s, StatusCode::BAD_GATEWAY);
    assert_eq!(w.status_of(order).await, "provisioned");
    let live: i64 =
        sqlx::query_scalar("SELECT count(*) FROM entitlements WHERE order_id = $1 AND status <> 'cancelled'").bind(order).fetch_one(&w.admin).await.unwrap();
    assert_eq!(live, 1, "the plan still works");
    let failed: i64 =
        sqlx::query_scalar("SELECT count(*) FROM refunds WHERE order_id = $1 AND status = 'failed'").bind(order).fetch_one(&w.admin).await.unwrap();
    assert_eq!(failed, 1, "the failure is on record");
    *w.bank.fail.lock().unwrap() = false;
    assert_eq!(w.call("POST", &format!("/admin/orders/{order}/refund"), Some(&owner), Some(json!({}))).await.0, StatusCode::OK);
    assert_eq!(w.status_of(order).await, "refunded");
}

#[tokio::test]
async fn simultaneous_clicks_refund_exactly_once() {
    let Some(w) = world().await else { return };
    let (_, order) = w.guest_order(10000).await;
    let owner = w.staff("o3@x.test", "system_admin").await;
    let mut hs = Vec::new();
    for _ in 0..6 {
        let (st, slug, t) = (w.state.clone(), w.slug.clone(), owner.clone());
        hs.push(tokio::spawn(async move {
            let req = Request::post(format!("/v1/t/{slug}/admin/orders/{order}/refund"))
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {t}"))
                .body(Body::from("{}"))
                .unwrap();
            router(st).oneshot(req).await.unwrap().status()
        }));
    }
    let mut ok = 0;
    for h in hs {
        if h.await.unwrap() == StatusCode::OK {
            ok += 1;
        }
    }
    assert_eq!(ok, 1);
    assert_eq!(w.bank.refunds.lock().unwrap().len(), 1, "Paystack was asked once");
}

#[tokio::test]
async fn a_plan_bought_from_the_wallet_is_refunded_to_the_wallet() {
    let Some(w) = world().await else { return };
    let (token, acct) = w.member("08041112222").await;
    sqlx::query("INSERT INTO wallet_ledger (tenant_id, account_id, kind, amount_kobo) VALUES ($1,$2,'adjustment',10000)")
        .bind(w.tenant)
        .bind(acct)
        .execute(&w.admin)
        .await
        .unwrap();
    let (s, v) = w.call("POST", "/members/wallet/buy", Some(&token), Some(json!({ "site": "site-1", "plan_id": w.plan_id }))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(w.balance(acct).await, 0);
    let order: Uuid = sqlx::query_scalar("SELECT id FROM orders WHERE account_id = $1").bind(acct).fetch_one(&w.admin).await.unwrap();
    let owner = w.staff("o4@x.test", "system_admin").await;
    let (s, v) = w.call("POST", &format!("/admin/orders/{order}/refund"), Some(&owner), Some(json!({}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["method"], "wallet");
    assert_eq!(w.balance(acct).await, 10000);
    assert!(w.bank.refunds.lock().unwrap().is_empty(), "no money left the platform");
}

#[tokio::test]
async fn a_topup_refund_needs_the_money_to_still_be_there() {
    let Some(w) = world().await else { return };
    let (token, acct) = w.member("08051112222").await;
    let mut orders = Vec::new();
    for _ in 0..2 {
        let (s, v) = w.call("POST", "/members/wallet/topup", Some(&token), Some(json!({ "site": "site-1", "amount_kobo": 20000 }))).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        let reference = v["reference"].as_str().unwrap().to_string();
        let tx = VerifiedTransaction {
            id: format!("t-{reference}"),
            reference: reference.clone(),
            status: "success".into(),
            amount_kobo: 20000,
            currency: "NGN".into(),
            channel: None,
            fees_kobo: 0,
            raw: json!({}),
        };
        fulfil_payment(&w.state.pool, w.tenant, &tx).await.unwrap();
        orders.push(sqlx::query_scalar::<_, Uuid>("SELECT id FROM orders WHERE reference = $1").bind(&reference).fetch_one(&w.admin).await.unwrap());
        // The status page works for a top-up too.
        assert_eq!(w.call("GET", &format!("/orders/{reference}"), None, None).await.1["plan"], "Wallet top-up");
    }
    assert_eq!(w.balance(acct).await, 40000);
    let owner = w.staff("o5@x.test", "system_admin").await;
    let (s, _) = w.call("POST", &format!("/admin/orders/{}/refund", orders[0]), Some(&owner), Some(json!({}))).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(w.balance(acct).await, 20000, "the refunded top-up is taken back from the wallet");
    // Spend most of what is left, then the second top-up cannot be refunded in full.
    sqlx::query("INSERT INTO wallet_ledger (tenant_id, account_id, kind, amount_kobo) VALUES ($1,$2,'purchase',-15000)")
        .bind(w.tenant)
        .bind(acct)
        .execute(&w.admin)
        .await
        .unwrap();
    let (s, _) = w.call("POST", &format!("/admin/orders/{}/refund", orders[1]), Some(&owner), Some(json!({}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(w.balance(acct).await, 5000, "nothing changed");
    assert_eq!(w.bank.refunds.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_payment_of_the_wrong_amount_is_refunded_for_what_was_paid() {
    let Some(w) = world().await else { return };
    let (reference, order) = w.guest_order(5000).await; // plan costs 10000
    assert_eq!(w.status_of(order).await, "refund_flagged");
    let owner = w.staff("o6@x.test", "system_admin").await;
    let (_, orders) = w.call("GET", "/admin/orders?status=refund_flagged", Some(&owner), None).await;
    assert_eq!(orders[0]["reference"], reference.as_str());
    let (s, v) = w.call("POST", &format!("/admin/orders/{order}/refund"), Some(&owner), Some(json!({}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["amount_kobo"], 5000);
    assert_eq!(*w.bank.refunds.lock().unwrap(), vec![(reference, 5000)]);
    assert_eq!(w.status_of(order).await, "refunded");
}

#[tokio::test]
async fn unpaid_and_foreign_orders_cannot_be_refunded() {
    let Some(w) = world().await else { return };
    let (s, v) = w.call("POST", "/orders", None, Some(json!({ "site": "site-1", "plan_id": w.plan_id, "phone": "08061112222" }))).await;
    assert_eq!(s, StatusCode::CREATED);
    let reference = v["reference"].as_str().unwrap();
    assert_eq!(w.call("POST", &format!("/orders/{reference}/refund"), None, Some(json!({}))).await.0, StatusCode::BAD_REQUEST, "not paid");
    assert_eq!(w.call("POST", "/orders/no-such-reference/refund", None, Some(json!({}))).await.0, StatusCode::NOT_FOUND);
    // Another tenant's owner cannot touch this tenant's order.
    let other = format!("r2-{}", &Uuid::new_v4().simple().to_string()[..8]);
    sqlx::query("INSERT INTO tenants (name, slug) VALUES ($1, $1)").bind(&other).execute(&w.admin).await.unwrap();
    platform_api::admin::create_admin(&w.state, &other, "x@o.test", "correct horse 42", "system_admin").await.unwrap();
    let login = Request::post(format!("/v1/t/{other}/admin/login"))
        .header("content-type", "application/json")
        .body(Body::from(json!({ "email": "x@o.test", "password": "correct horse 42" }).to_string()))
        .unwrap();
    let resp = router(w.state.clone()).oneshot(login).await.unwrap();
    let t: Value = serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let order: Uuid = sqlx::query_scalar("SELECT id FROM orders WHERE reference = $1").bind(reference).fetch_one(&w.admin).await.unwrap();
    let req = Request::post(format!("/v1/t/{other}/admin/orders/{order}/refund"))
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {}", t["token"].as_str().unwrap()))
        .body(Body::from("{}"))
        .unwrap();
    assert_eq!(router(w.state.clone()).oneshot(req).await.unwrap().status(), StatusCode::NOT_FOUND);
}
