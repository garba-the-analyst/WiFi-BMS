//! Roles, staff management, 2-step verification (app, email, text message), manual and special
//! vouchers, customisable plans, complaints, the error log and tenant provisioning.
//! Real PostgreSQL with row-level security on. See `run_integration.sh`; skipped without the env vars.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use platform_api::auth::RateLimiter;
use platform_api::error::{AppError, Result};
use platform_api::http::router;
use platform_api::mail::{DisabledMailer, Mailer, MemoryMailer};
use platform_api::paystack::{Gateway, InitializeRequest, InitializedTransaction, VerifiedTransaction};
use platform_api::secrets::MasterKey;
use platform_api::sms::MemorySms;
use platform_api::{totp, AppState, Config};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const PW: &str = "correct horse 42";

struct Down;
#[async_trait]
impl Gateway for Down {
    async fn initialize(&self, _: InitializeRequest<'_>) -> Result<InitializedTransaction> {
        Err(AppError::Gateway("paystack timed out".into()))
    }
    async fn verify(&self, _: &str, _: &str) -> Result<VerifiedTransaction> {
        Err(AppError::Gateway("unused".into()))
    }
}

struct World {
    admin: PgPool,
    state: AppState,
    mail: MemoryMailer,
    sms: MemorySms,
    slug: String,
    tenant: Uuid,
    site_id: Uuid,
    plan: Uuid,
}

async fn world_with(require_2fa: bool, mail_on: bool) -> Option<World> {
    let (Ok(app_url), Ok(admin_url)) = (std::env::var("DATABASE_URL"), std::env::var("ADMIN_DATABASE_URL")) else {
        eprintln!("NOTICE: DATABASE_URL / ADMIN_DATABASE_URL not set; skipping integration test");
        return None;
    };
    let app = PgPoolOptions::new().max_connections(5).connect(&app_url).await.unwrap();
    let admin = PgPoolOptions::new().max_connections(2).connect(&admin_url).await.unwrap();
    let slug = format!("s-{}", &Uuid::new_v4().simple().to_string()[..10]);
    let tenant: Uuid = sqlx::query_scalar("INSERT INTO tenants (name, slug) VALUES ($1, $1) RETURNING id").bind(&slug).fetch_one(&admin).await.unwrap();
    let site_id: Uuid = sqlx::query_scalar("INSERT INTO sites (tenant_id, name, slug) VALUES ($1, 'Site 1', 'site-1') RETURNING id")
        .bind(tenant)
        .fetch_one(&admin)
        .await
        .unwrap();
    let profile: Uuid = sqlx::query_scalar("INSERT INTO speed_profiles (tenant_id, name, down_kbps, up_kbps) VALUES ($1, 'Std', 2000, 2000) RETURNING id")
        .bind(tenant)
        .fetch_one(&admin)
        .await
        .unwrap();
    let plan: Uuid = sqlx::query_scalar(
        "INSERT INTO plans (tenant_id, name, kind, price_kobo, active_seconds, valid_seconds, speed_profile_id) VALUES ($1, '1 Hour', 'time', 20000, 3600, 86400, $2) RETURNING id",
    )
    .bind(tenant)
    .bind(profile)
    .fetch_one(&admin)
    .await
    .unwrap();
    sqlx::query("INSERT INTO tenant_secrets (tenant_id, paystack_secret_enc) VALUES ($1, $2)")
        .bind(tenant)
        .bind(MasterKey::from_hex(&"00".repeat(32)).unwrap().encrypt("sk_test_x").unwrap())
        .execute(&admin)
        .await
        .unwrap();
    let (mail, sms) = (MemoryMailer::default(), MemorySms::default());
    let mailer: Arc<dyn Mailer> = if mail_on { Arc::new(mail.clone()) } else { Arc::new(DisabledMailer) };
    let state = AppState {
        pool: app,
        gateway: Arc::new(Down),
        master_key: MasterKey::from_hex(&"00".repeat(32)).unwrap(),
        config: Config { public_base_url: "https://wifi.test".into(), fallback_email_domain: "guest.test".into(), trust_proxy: false, require_2fa },
        limiter: Arc::new(RateLimiter::new()),
        sms: Arc::new(sms.clone()),
        mailer,
    };
    Some(World { admin, state, mail, sms, slug, tenant, site_id, plan })
}

async fn world() -> Option<World> {
    world_with(false, true).await
}

impl World {
    async fn req(&self, method: &str, path: &str, token: Option<&str>, body: Option<Value>) -> (StatusCode, Value, String) {
        let mut b = Request::builder().method(method).uri(path).header("content-type", "application/json");
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        let resp = router(self.state.clone()).oneshot(b.body(body.map(|v| Body::from(v.to_string())).unwrap_or_else(Body::empty)).unwrap()).await.unwrap();
        let st = resp.status();
        let text = String::from_utf8(resp.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
        (st, serde_json::from_str(&text).unwrap_or(Value::Null), text)
    }
    async fn call(&self, method: &str, path: &str, token: Option<&str>, body: Option<Value>) -> (StatusCode, Value, String) {
        self.req(method, &format!("/v1/t/{}/admin{}", self.slug, path), token, body).await
    }
    async fn public(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value, String) {
        self.req(method, &format!("/v1/t/{}{}", self.slug, path), None, body).await
    }
    /// Create a staff account and sign in (works only when 2-step is not required, or returns the limited token).
    async fn staff(&self, email: &str, role: &str) -> String {
        platform_api::admin::create_admin(&self.state, &self.slug, email, PW, role).await.unwrap();
        let (s, v, _) = self.call("POST", "/login", None, Some(json!({ "email": email, "password": PW }))).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["token"].as_str().unwrap().to_string()
    }
    async fn connect_code(&self, code: &str) -> StatusCode {
        self.public("POST", "/connect", Some(json!({ "site": "site-1", "mac": "AA:BB:CC:00:11:33", "code": code, "accept_terms": true }))).await.0
    }
    fn last_mail_code(&self) -> String {
        let m = self.mail.outbox.lock().unwrap();
        let text = &m.last().expect("an email was sent").2;
        text.split("code is ").nth(1).unwrap()[..6].to_string()
    }
    fn last_sms_code(&self) -> String {
        let m = self.sms.outbox.lock().unwrap();
        let text = &m.last().expect("a text was sent").1;
        text.split("code is ").nth(1).unwrap()[..6].to_string()
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

// ---------------------------------------------------------------------------
// Roles
// ---------------------------------------------------------------------------

#[tokio::test]
async fn each_role_reaches_only_what_it_should() {
    let Some(w) = world().await else { return };
    let sys = w.staff("sys@x.test", "system_admin").await;
    let ceo = w.staff("ceo@x.test", "ceo").await;
    let adm = w.staff("adm@x.test", "admin").await;
    let rep = w.staff("rep@x.test", "customer_rep").await;
    // (method, path, body, [sys, ceo, admin, rep] allowed?)
    let special = json!({ "label": "Test", "site": "site-1" });
    let cases: Vec<(&str, &str, Option<Value>, [bool; 4])> = vec![
        ("GET", "/overview", None, [true, true, true, false]),
        ("GET", "/customers", None, [true, true, true, true]),
        ("GET", "/plans", None, [true, true, true, true]),
        ("GET", "/orders", None, [true, true, true, false]),
        ("GET", "/users", None, [true, true, false, false]),
        ("GET", "/audit", None, [true, true, false, false]),
        ("POST", "/node-tokens", Some(json!({ "site": "site-1", "name": "pc" })), [true, false, false, false]),
        ("GET", "/errors", None, [true, true, true, true]),
        ("GET", "/complaints", None, [true, true, true, true]),
        ("GET", "/vouchers", None, [true, true, true, true]),
        ("POST", "/vouchers/special", Some(special), [true, true, true, false]),
        ("PUT", "/brand", Some(json!({ "tagline": "hi" })), [true, true, false, false]),
        ("PUT", "/secrets/paystack", Some(json!({ "secret": "sk_test_abcdefghijklmnop" })), [true, true, false, false]),
    ];
    for (method, path, body, allowed) in cases {
        for (i, (who, token)) in [("system_admin", &sys), ("ceo", &ceo), ("admin", &adm), ("customer_rep", &rep)].iter().enumerate() {
            let (s, v, _) = w.call(method, path, Some(token), body.clone()).await;
            if allowed[i] {
                assert_ne!(s, StatusCode::FORBIDDEN, "{who} should reach {method} {path}: {v}");
                assert_ne!(s, StatusCode::UNAUTHORIZED, "{who} {method} {path}");
            } else {
                assert_eq!(s, StatusCode::FORBIDDEN, "{who} must NOT reach {method} {path}");
            }
        }
    }
    // /me tells the app what to show.
    let (_, me, _) = w.call("GET", "/me", Some(&rep), None).await;
    assert_eq!(me["role"], "customer_rep");
    let perms: Vec<&str> = me["permissions"].as_array().unwrap().iter().map(|p| p.as_str().unwrap()).collect();
    assert!(perms.contains(&"complaints") && perms.contains(&"manual_sale") && !perms.contains(&"special_voucher"));
}

#[tokio::test]
async fn staff_management_follows_the_hierarchy() {
    let Some(w) = world().await else { return };
    let sys = w.staff("sys@x.test", "system_admin").await;
    let ceo = w.staff("ceo@x.test", "ceo").await;
    let adm = w.staff("adm@x.test", "admin").await;
    let new = |email: &str, role: &str| json!({ "email": email, "password": PW, "role": role, "display_name": "Person" });
    // system admin creates anyone
    for (e, r) in [("a1@x.test", "admin"), ("r1@x.test", "customer_rep"), ("c2@x.test", "ceo")] {
        assert_eq!(w.call("POST", "/users", Some(&sys), Some(new(e, r))).await.0, StatusCode::OK, "{r}");
    }
    // CEO creates admins and reps, not CEOs or system admins
    assert_eq!(w.call("POST", "/users", Some(&ceo), Some(new("r2@x.test", "customer_rep"))).await.0, StatusCode::OK);
    assert_eq!(w.call("POST", "/users", Some(&ceo), Some(new("a2@x.test", "admin"))).await.0, StatusCode::OK);
    assert_eq!(w.call("POST", "/users", Some(&ceo), Some(new("c3@x.test", "ceo"))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(w.call("POST", "/users", Some(&ceo), Some(new("s2@x.test", "system_admin"))).await.0, StatusCode::FORBIDDEN);
    // admins manage nobody
    assert_eq!(w.call("POST", "/users", Some(&adm), Some(new("x1@x.test", "customer_rep"))).await.0, StatusCode::FORBIDDEN);
    // duplicates and weak passwords are refused
    assert_eq!(w.call("POST", "/users", Some(&sys), Some(new("a1@x.test", "admin"))).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(
        w.call("POST", "/users", Some(&sys), Some(json!({ "email": "w@x.test", "password": "short", "role": "admin" }))).await.0,
        StatusCode::BAD_REQUEST
    );

    let (_, list, _) = w.call("GET", "/users", Some(&sys), None).await;
    let id_of = |email: &str| list.as_array().unwrap().iter().find(|u| u["email"] == email).unwrap()["id"].as_str().unwrap().to_string();
    let (a1, c2, r2) = (id_of("a1@x.test"), id_of("c2@x.test"), id_of("r2@x.test"));
    // CEO cannot touch another CEO or promote anyone to CEO
    assert_eq!(w.call("PUT", &format!("/users/{c2}"), Some(&ceo), Some(json!({ "status": "suspended" }))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(w.call("PUT", &format!("/users/{a1}"), Some(&ceo), Some(json!({ "role": "ceo" }))).await.0, StatusCode::FORBIDDEN);
    // but may change an admin to a rep
    assert_eq!(w.call("PUT", &format!("/users/{a1}"), Some(&ceo), Some(json!({ "role": "customer_rep" }))).await.0, StatusCode::OK);

    // delete closes the account and ends its sessions
    let r2_token = {
        let (_, v, _) = w.call("POST", "/login", None, Some(json!({ "email": "r2@x.test", "password": PW }))).await;
        v["token"].as_str().unwrap().to_string()
    };
    assert_eq!(w.call("GET", "/complaints", Some(&r2_token), None).await.0, StatusCode::OK);
    assert_eq!(w.call("DELETE", &format!("/users/{r2}"), Some(&ceo), None).await.0, StatusCode::OK);
    assert_eq!(w.call("GET", "/complaints", Some(&r2_token), None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(w.call("POST", "/login", None, Some(json!({ "email": "r2@x.test", "password": PW }))).await.0, StatusCode::UNAUTHORIZED);
    let (_, list, _) = w.call("GET", "/users", Some(&sys), None).await;
    assert!(list.as_array().unwrap().iter().all(|u| u["email"] != "r2@x.test"), "closed accounts are hidden");
    let (_, all, _) = w.call("GET", "/users?status=all", Some(&sys), None).await;
    assert!(all.as_array().unwrap().iter().any(|u| u["email"] == "r2@x.test" && u["status"] == "closed"));

    // the last system administrator can neither be deleted, demoted, nor suspended; nobody deletes themselves
    let (_, me, _) = w.call("GET", "/me", Some(&sys), None).await;
    let sys_id = me["id"].as_str().unwrap().to_string();
    assert_eq!(w.call("DELETE", &format!("/users/{sys_id}"), Some(&sys), None).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(w.call("PUT", &format!("/users/{sys_id}"), Some(&sys), Some(json!({ "role": "ceo" }))).await.0, StatusCode::BAD_REQUEST);

    // their actions show up in the activity view, for system admin and CEO only
    let (_, act, _) = w.call("GET", &format!("/users/{sys_id}/activity"), Some(&ceo), None).await;
    assert!(act.as_array().unwrap().iter().any(|e| e["action"] == "staff.created"), "{act}");
    assert_eq!(w.call("GET", &format!("/users/{sys_id}/activity"), Some(&adm), None).await.0, StatusCode::FORBIDDEN);
    let (_, trail, _) = w.call("GET", &format!("/audit?actor={sys_id}"), Some(&sys), None).await;
    assert!(trail.as_array().unwrap().iter().all(|e| e["actor_id"] == sys_id.as_str()));
    assert!(trail[0]["actor_email"] == "sys@x.test");

    // password change needs the current one
    assert_eq!(
        w.call("PUT", "/password", Some(&adm), Some(json!({ "current": "wrong wrong 9", "new": "another long pass 77" }))).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(w.call("PUT", "/password", Some(&adm), Some(json!({ "current": PW, "new": "another long pass 77" }))).await.0, StatusCode::OK);
    assert_eq!(w.call("POST", "/login", None, Some(json!({ "email": "adm@x.test", "password": "another long pass 77" }))).await.0, StatusCode::OK);
}

// ---------------------------------------------------------------------------
// 2-step verification
// ---------------------------------------------------------------------------

#[tokio::test]
async fn two_step_is_required_before_anything_else_and_totp_works() {
    let Some(w) = world_with(true, true).await else { return };
    platform_api::admin::create_admin(&w.state, &w.slug, "boss@x.test", PW, "system_admin").await.unwrap();
    let (s, v, _) = w.call("POST", "/login", None, Some(json!({ "email": "boss@x.test", "password": PW }))).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["setup_required"], true);
    let limited = v["token"].as_str().unwrap().to_string();
    // The limited token reaches nothing but setup.
    let (s, v, _) = w.call("GET", "/overview", Some(&limited), None).await;
    assert_eq!((s, v["code"].as_str()), (StatusCode::FORBIDDEN, Some("mfa_required")));
    assert_eq!(w.call("GET", "/users", Some(&limited), None).await.0, StatusCode::FORBIDDEN);
    let (s, me, _) = w.call("GET", "/me", Some(&limited), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(me["setup_required"], true);
    assert_eq!(me["methods_available"]["totp"], true);

    // Start the app method; a wrong code does not enable it.
    let (s, start, _) = w.call("POST", "/2fa/start", Some(&limited), Some(json!({ "method": "totp" }))).await;
    assert_eq!(s, StatusCode::OK, "{start}");
    assert!(start["otpauth_uri"].as_str().unwrap().starts_with("otpauth://totp/"));
    let secret = totp::base32_decode(start["secret"].as_str().unwrap()).unwrap();
    let ch = start["challenge"].as_str().unwrap().to_string();
    assert_eq!(w.call("POST", "/2fa/confirm", Some(&limited), Some(json!({ "challenge": ch, "code": "000000" }))).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(
        w.call("POST", "/2fa/confirm", Some(&limited), Some(json!({ "challenge": ch, "code": totp::code_at(&secret, now(), 6) }))).await.0,
        StatusCode::OK
    );
    // The same token now works for everything it is allowed to do.
    assert_eq!(w.call("GET", "/users", Some(&limited), None).await.0, StatusCode::OK);

    // From now on: password alone gives only a challenge, never a token.
    let (s, v, _) = w.call("POST", "/login", None, Some(json!({ "email": "boss@x.test", "password": PW }))).await;
    assert_eq!(s, StatusCode::OK);
    assert!(v.get("token").is_none(), "{v}");
    assert_eq!(v["method"], "totp");
    let ch = v["challenge"].as_str().unwrap().to_string();
    assert_eq!(w.call("POST", "/login/verify", None, Some(json!({ "challenge": ch, "code": "123456" }))).await.0, StatusCode::UNAUTHORIZED);
    let (s, v, _) = w.call("POST", "/login/verify", None, Some(json!({ "challenge": ch, "code": totp::code_at(&secret, now(), 6) }))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["mfa_method"], "totp");
    // A challenge is single use.
    assert_eq!(
        w.call("POST", "/login/verify", None, Some(json!({ "challenge": ch, "code": totp::code_at(&secret, now(), 6) }))).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn email_codes_enrol_and_sign_in_and_die_after_five_wrong_tries() {
    let Some(w) = world_with(true, true).await else { return };
    platform_api::admin::create_admin(&w.state, &w.slug, "mail@x.test", PW, "admin").await.unwrap();
    let (_, v, _) = w.call("POST", "/login", None, Some(json!({ "email": "mail@x.test", "password": PW }))).await;
    let limited = v["token"].as_str().unwrap().to_string();
    let (s, start, _) = w.call("POST", "/2fa/start", Some(&limited), Some(json!({ "method": "email" }))).await;
    assert_eq!(s, StatusCode::OK, "{start}");
    assert_eq!(start["sent_to"], "m***@x.test");
    let code = w.last_mail_code();
    assert_eq!(w.call("POST", "/2fa/confirm", Some(&limited), Some(json!({ "challenge": start["challenge"], "code": code }))).await.0, StatusCode::OK);

    let (_, v, _) = w.call("POST", "/login", None, Some(json!({ "email": "mail@x.test", "password": PW }))).await;
    assert_eq!(v["method"], "email");
    let ch = v["challenge"].as_str().unwrap().to_string();
    let good = w.last_mail_code();
    // five wrong tries kill the challenge, even for the right code afterwards
    for _ in 0..5 {
        assert_eq!(w.call("POST", "/login/verify", None, Some(json!({ "challenge": ch, "code": "000000" }))).await.0, StatusCode::UNAUTHORIZED);
    }
    assert_eq!(w.call("POST", "/login/verify", None, Some(json!({ "challenge": ch, "code": good }))).await.0, StatusCode::UNAUTHORIZED);
    // five wrong codes also count as failed sign-ins: the account is locked, like after five wrong passwords
    assert_eq!(w.call("POST", "/login", None, Some(json!({ "email": "mail@x.test", "password": PW }))).await.0, StatusCode::TOO_MANY_REQUESTS);
    sqlx::query("UPDATE admin_users SET locked_until = NULL, failed_logins = 0 WHERE tenant_id = $1").bind(w.tenant).execute(&w.admin).await.unwrap();
    // a fresh sign-in sends a new code that works, and the old challenge's code does not work on it
    let (_, v2, _) = w.call("POST", "/login", None, Some(json!({ "email": "mail@x.test", "password": PW }))).await;
    let ch2 = v2["challenge"].as_str().unwrap().to_string();
    let good2 = w.last_mail_code();
    let (s, v, _) = w.call("POST", "/login/verify", None, Some(json!({ "challenge": ch2, "code": good2 }))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["token"].as_str().unwrap().starts_with("at_"));
    // the code is stored hashed, never in clear
    let stored: Vec<Option<String>> =
        sqlx::query_scalar("SELECT code_hash FROM admin_challenges WHERE tenant_id = $1").bind(w.tenant).fetch_all(&w.admin).await.unwrap();
    assert!(stored.iter().flatten().all(|h| h.len() == 64 && h != &good2));
}

#[tokio::test]
async fn text_message_codes_work_and_unset_providers_are_refused_plainly() {
    let Some(w) = world_with(true, true).await else { return };
    platform_api::admin::create_admin(&w.state, &w.slug, "sms@x.test", PW, "ceo").await.unwrap();
    let (_, v, _) = w.call("POST", "/login", None, Some(json!({ "email": "sms@x.test", "password": PW }))).await;
    let limited = v["token"].as_str().unwrap().to_string();
    // a phone number is required
    assert_eq!(w.call("POST", "/2fa/start", Some(&limited), Some(json!({ "method": "sms" }))).await.0, StatusCode::BAD_REQUEST);
    let (s, start, _) = w.call("POST", "/2fa/start", Some(&limited), Some(json!({ "method": "sms", "phone": "08031234567" }))).await;
    assert_eq!(s, StatusCode::OK, "{start}");
    assert_eq!(start["sent_to"], "080***567");
    assert_eq!(w.sms.outbox.lock().unwrap().last().unwrap().0, "08031234567");
    let code = w.last_sms_code();
    assert_eq!(w.call("POST", "/2fa/confirm", Some(&limited), Some(json!({ "challenge": start["challenge"], "code": code }))).await.0, StatusCode::OK);
    let (_, v, _) = w.call("POST", "/login", None, Some(json!({ "email": "sms@x.test", "password": PW }))).await;
    assert_eq!(v["method"], "sms");
    assert_eq!(v["sent_to"], "080***567");
    let (s, v, _) = w.call("POST", "/login/verify", None, Some(json!({ "challenge": v["challenge"], "code": w.last_sms_code() }))).await;
    assert_eq!(s, StatusCode::OK, "{v}");

    // email codes cannot be chosen when no mail server is configured
    let Some(w2) = world_with(true, false).await else { return };
    platform_api::admin::create_admin(&w2.state, &w2.slug, "nomail@x.test", PW, "admin").await.unwrap();
    let (_, v, _) = w2.call("POST", "/login", None, Some(json!({ "email": "nomail@x.test", "password": PW }))).await;
    let t = v["token"].as_str().unwrap().to_string();
    let (_, me, _) = w2.call("GET", "/me", Some(&t), None).await;
    assert_eq!(me["methods_available"]["email"], false);
    let (s, v, _) = w2.call("POST", "/2fa/start", Some(&t), Some(json!({ "method": "email" }))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
}

#[tokio::test]
async fn resetting_someones_two_step_forces_them_to_enrol_again() {
    let Some(w) = world().await else { return };
    let sys = w.staff("sys@x.test", "system_admin").await;
    let rep_token = w.staff("rep@x.test", "customer_rep").await;
    let (_, list, _) = w.call("GET", "/users", Some(&sys), None).await;
    let rep = list.as_array().unwrap().iter().find(|u| u["email"] == "rep@x.test").unwrap()["id"].as_str().unwrap().to_string();
    sqlx::query("UPDATE admin_users SET mfa_method = 'email' WHERE id = $1::uuid").bind(&rep).execute(&w.admin).await.unwrap();
    assert_eq!(w.call("POST", &format!("/users/{rep}/reset-2fa"), Some(&sys), None).await.0, StatusCode::OK);
    assert_eq!(w.call("GET", "/complaints", Some(&rep_token), None).await.0, StatusCode::UNAUTHORIZED, "sessions end");
    let method: String = sqlx::query_scalar("SELECT mfa_method FROM admin_users WHERE id = $1::uuid").bind(&rep).fetch_one(&w.admin).await.unwrap();
    assert_eq!(method, "none");
    assert_eq!(w.call("POST", &format!("/users/{rep}/reset-password"), Some(&sys), Some(json!({ "password": "a brand new pass 55" }))).await.0, StatusCode::OK);
    assert_eq!(w.call("POST", "/login", None, Some(json!({ "email": "rep@x.test", "password": "a brand new pass 55" }))).await.0, StatusCode::OK);
}

// ---------------------------------------------------------------------------
// Plans
// ---------------------------------------------------------------------------

#[tokio::test]
async fn plans_are_customisable_and_bandwidth_defaults_to_two_mbps() {
    let Some(w) = world().await else { return };
    let adm = w.staff("adm@x.test", "admin").await;
    // hourly time plan, nothing said about speed or devices: 2 Mbps both ways, one device
    let (s, v, _) = w
        .call("POST", "/plans", Some(&adm), Some(json!({ "name": "2 Hours", "kind": "time", "price_kobo": 30000, "duration": { "unit": "hour", "count": 2 } })))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (_, plans, _) = w.call("GET", "/plans", Some(&adm), None).await;
    let p = plans.as_array().unwrap().iter().find(|p| p["name"] == "2 Hours").unwrap();
    assert_eq!((p["active_seconds"].as_i64(), p["valid_seconds"].as_i64()), (Some(7200), Some(86_400)));
    assert_eq!((p["down_kbps"].as_i64(), p["up_kbps"].as_i64(), p["device_limit"].as_i64()), (Some(2000), Some(2000), Some(1)));
    // weekly plan for three devices at 5 Mbps down / 2 Mbps up: a profile is made, and shared by the next plan that asks
    let body = |name: &str| json!({ "name": name, "kind": "time", "price_kobo": 500000, "duration": { "unit": "week", "count": 1 }, "device_limit": 3, "down_kbps": 5000, "up_kbps": 2000 });
    assert_eq!(w.call("POST", "/plans", Some(&adm), Some(body("Weekly A"))).await.0, StatusCode::OK);
    assert_eq!(w.call("POST", "/plans", Some(&adm), Some(body("Weekly B"))).await.0, StatusCode::OK);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM speed_profiles WHERE tenant_id = $1 AND down_kbps = 5000 AND up_kbps = 2000")
        .bind(w.tenant)
        .fetch_one(&w.admin)
        .await
        .unwrap();
    assert_eq!(n, 1);
    // data plan: size, expiry, devices, bandwidth
    let (s, v, _) = w
        .call("POST", "/plans", Some(&adm), Some(json!({ "name": "5 GB", "kind": "data", "price_kobo": 250000, "data_bytes": 5_368_709_120i64, "valid_for": { "unit": "day", "count": 7 }, "device_limit": 2 })))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (_, plans, _) = w.call("GET", "/plans", Some(&adm), None).await;
    let d = plans.as_array().unwrap().iter().find(|p| p["name"] == "5 GB").unwrap();
    assert_eq!(
        (d["data_bytes"].as_i64(), d["valid_seconds"].as_i64(), d["active_seconds"].is_null(), d["down_kbps"].as_i64()),
        (Some(5_368_709_120), Some(604_800), true, Some(2000))
    );
    // a data plan with no data, and nonsense speeds, are refused
    assert_eq!(
        w.call("POST", "/plans", Some(&adm), Some(json!({ "name": "bad", "kind": "data", "price_kobo": 1, "valid_for": { "unit": "day", "count": 1 } })))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        w.call(
            "POST",
            "/plans",
            Some(&adm),
            Some(json!({ "name": "bad", "kind": "time", "price_kobo": 1, "duration": { "unit": "day", "count": 1 }, "down_kbps": 5 }))
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        w.call("POST", "/plans", Some(&adm), Some(json!({ "name": "bad", "kind": "time", "price_kobo": 1, "duration": { "unit": "year", "count": 1 } })))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        w.call(
            "POST",
            "/plans",
            Some(&adm),
            Some(json!({ "name": "bad", "kind": "time", "price_kobo": 1, "duration": { "unit": "day", "count": 1 }, "device_limit": 0 }))
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}

// ---------------------------------------------------------------------------
// Vouchers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn front_desk_sells_manual_vouchers_at_list_price_only() {
    let Some(w) = world().await else { return };
    let rep = w.staff("rep@x.test", "customer_rep").await;
    let (s, v, _) = w
        .call(
            "POST",
            "/vouchers/manual",
            Some(&rep),
            Some(json!({ "plan_id": w.plan, "site": "site-1", "phone": "08031234567", "label": "Walk-in", "method": "cash" })),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["amount_kobo"], 20000, "the price comes from the plan");
    let code = v["access_code"].as_str().unwrap().replace(' ', "");
    assert_eq!(code.len(), 8);
    let (kind, issuer, label): (String, Option<Uuid>, Option<String>) =
        sqlx::query_as("SELECT kind, issued_by, label FROM access_codes WHERE tenant_id = $1 AND code = $2")
            .bind(w.tenant)
            .bind(&code)
            .fetch_one(&w.admin)
            .await
            .unwrap();
    assert_eq!((kind.as_str(), issuer.is_some(), label.as_deref()), ("manual", true, Some("Walk-in")));
    let (paid_with, status, amount): (String, String, i64) = sqlx::query_as("SELECT paid_with, status::text, amount_kobo FROM orders WHERE reference = $1")
        .bind(v["reference"].as_str().unwrap())
        .fetch_one(&w.admin)
        .await
        .unwrap();
    assert_eq!((paid_with.as_str(), status.as_str(), amount), ("cash", "provisioned", 20000));
    // the walk-in can use the code on the portal
    let (s, t, _) = w.public("POST", "/connect", Some(json!({ "site": "site-1", "mac": "AA:BB:CC:00:11:22", "code": code, "accept_terms": true }))).await;
    assert_eq!(s, StatusCode::OK, "{t}");
    // sale to a member's account
    let acct: Uuid = sqlx::query_scalar("INSERT INTO accounts (tenant_id, phone, display_name) VALUES ($1, '+2348022223333', 'Amina') RETURNING id")
        .bind(w.tenant)
        .fetch_one(&w.admin)
        .await
        .unwrap();
    let (s, v, _) = w.call("POST", "/vouchers/manual", Some(&rep), Some(json!({ "plan_id": w.plan, "site": "site-1", "account_id": acct }))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["access_code"].is_null());
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM entitlements WHERE account_id = $1").bind(acct).fetch_one(&w.admin).await.unwrap();
    assert_eq!(n, 1);
    // unknown method, hidden plans and unknown sites are refused
    assert_eq!(
        w.call("POST", "/vouchers/manual", Some(&rep), Some(json!({ "plan_id": w.plan, "site": "site-1", "method": "barter" }))).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(w.call("POST", "/vouchers/manual", Some(&rep), Some(json!({ "plan_id": w.plan, "site": "nowhere" }))).await.0, StatusCode::BAD_REQUEST);
    sqlx::query("UPDATE plans SET is_active = false WHERE id = $1").bind(w.plan).execute(&w.admin).await.unwrap();
    assert_eq!(w.call("POST", "/vouchers/manual", Some(&rep), Some(json!({ "plan_id": w.plan, "site": "site-1" }))).await.0, StatusCode::BAD_REQUEST);
    // the sale is in the audit trail with who took it
    let (_, trail, _) = w.call("GET", "/audit?action=voucher.", Some(&w.staff("sys@x.test", "system_admin").await), None).await;
    assert!(trail.as_array().unwrap().iter().any(|e| e["action"] == "voucher.manual_sale" && e["actor_email"] == "rep@x.test"));
}

#[tokio::test]
async fn special_vouchers_give_unlimited_access_to_non_reps_only_and_can_be_revoked() {
    let Some(w) = world().await else { return };
    let adm = w.staff("adm@x.test", "admin").await;
    let rep = w.staff("rep@x.test", "customer_rep").await;
    let body = json!({ "label": "Chairman's guests", "site": "site-1", "devices": 3, "count": 2, "down_kbps": 10000, "up_kbps": 5000 });
    assert_eq!(w.call("POST", "/vouchers/special", Some(&rep), Some(body.clone())).await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        w.call("POST", "/vouchers/special", Some(&adm), Some(json!({ "label": "  ", "site": "site-1" }))).await.0,
        StatusCode::BAD_REQUEST,
        "a label is required"
    );
    assert_eq!(w.call("POST", "/vouchers/special", Some(&adm), Some(json!({ "label": "x", "site": "site-1", "count": 51 }))).await.0, StatusCode::BAD_REQUEST);
    let (s, v, _) = w.call("POST", "/vouchers/special", Some(&adm), Some(body)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["ends"], "never");
    let codes: Vec<String> = v["vouchers"].as_array().unwrap().iter().map(|c| c["access_code"].as_str().unwrap().replace(' ', "")).collect();
    assert_eq!(codes.len(), 2);
    // terms: unlimited (no price, no budget beyond the window), 3 devices, own speed, hidden from the price list
    let (price, special, active, devices, down): (i64, bool, bool, i16, i32) = sqlx::query_as(
        "SELECT p.price_kobo, p.is_special, p.is_active, p.device_limit, s.down_kbps FROM access_codes c JOIN entitlements e ON e.access_code_id = c.id JOIN plans p ON p.id = e.plan_id JOIN speed_profiles s ON s.id = p.speed_profile_id WHERE c.code = $1",
    )
    .bind(&codes[0])
    .fetch_one(&w.admin)
    .await
    .unwrap();
    assert_eq!((price, special, active, devices, down), (0, true, false, 3, 10000));
    let (_, public, _) = w.public("GET", "/sites/site-1/plans", None).await;
    assert!(public.as_array().unwrap().iter().all(|p| !p["name"].as_str().unwrap().starts_with("Special")), "never on the price list");
    let (_, admin_plans, _) = w.call("GET", "/plans", Some(&adm), None).await;
    assert!(admin_plans.as_array().unwrap().iter().all(|p| !p["name"].as_str().unwrap().starts_with("Special")));
    // it works, then reps cannot see it, then it is revoked and stops working

    assert_eq!(w.connect_code(&codes[0]).await, StatusCode::OK);
    let (_, rep_list, _) = w.call("GET", "/vouchers", Some(&rep), None).await;
    assert!(rep_list.as_array().unwrap().is_empty(), "reps do not see special vouchers");
    assert_eq!(w.call("GET", "/vouchers?kind=special", Some(&rep), None).await.0, StatusCode::FORBIDDEN);
    let (_, adm_list, _) = w.call("GET", "/vouchers?kind=special", Some(&adm), None).await;
    assert_eq!(adm_list.as_array().unwrap().len(), 2);
    let id = adm_list.as_array().unwrap().iter().find(|c| c["code"] == codes[1].as_str()).unwrap()["id"].as_str().unwrap().to_string();
    assert_eq!(w.call("POST", &format!("/vouchers/{id}/revoke"), Some(&rep), None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(w.call("POST", &format!("/vouchers/{id}/revoke"), Some(&adm), None).await.0, StatusCode::OK);
    assert_ne!(w.connect_code(&codes[1]).await, StatusCode::OK, "a revoked voucher no longer connects");
    assert_eq!(w.connect_code(&codes[0]).await, StatusCode::OK, "the other one is untouched");
    // with an end date
    let (_, v, _) = w
        .call("POST", "/vouchers/special", Some(&adm), Some(json!({ "label": "Contractor", "site": "site-1", "valid_for": { "unit": "week", "count": 2 } })))
        .await;
    assert_eq!(v["ends"], "after the chosen period from first use");
    let (valid,): (i32,) = sqlx::query_as("SELECT valid_seconds FROM plans WHERE name LIKE 'Special: Contractor%'").fetch_one(&w.admin).await.unwrap();
    assert_eq!(valid, 14 * 86_400);
}

// ---------------------------------------------------------------------------
// Complaints
// ---------------------------------------------------------------------------

#[tokio::test]
async fn customers_complain_and_reps_work_the_queue() {
    let Some(w) = world().await else { return };
    let rep = w.staff("rep@x.test", "customer_rep").await;
    let acct: Uuid = sqlx::query_scalar("INSERT INTO accounts (tenant_id, phone, display_name) VALUES ($1, '+2348022223333', 'Amina') RETURNING id")
        .bind(w.tenant)
        .fetch_one(&w.admin)
        .await
        .unwrap();
    assert_eq!(w.public("POST", "/complaints", Some(json!({ "phone": "123", "message": "no internet" }))).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(w.public("POST", "/complaints", Some(json!({ "phone": "08022223333", "message": "x" }))).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(
        w.public("POST", "/complaints", Some(json!({ "phone": "08022223333", "message": "hello there", "category": "bogus" }))).await.0,
        StatusCode::BAD_REQUEST
    );
    let (s, v, _) =
        w.public("POST", "/complaints", Some(json!({ "phone": "0802 222 3333", "message": "I paid but I am not connected", "category": "payment" }))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["reference"].as_str().unwrap().len(), 8);
    // staff only
    assert_eq!(w.req("GET", &format!("/v1/t/{}/admin/complaints", w.slug), None, None).await.0, StatusCode::UNAUTHORIZED);
    let (_, list, _) = w.call("GET", "/complaints?status=open", Some(&rep), None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["is_member"], true, "linked to the member by phone number");
    let id = list[0]["id"].as_str().unwrap().to_string();
    let (_, d, _) = w.call("GET", &format!("/complaints/{id}"), Some(&rep), None).await;
    assert_eq!(d["customer"]["id"], acct.to_string());
    assert_eq!(
        w.call("POST", &format!("/complaints/{id}/notes"), Some(&rep), Some(json!({ "body": "Called the customer, checking the payment" }))).await.0,
        StatusCode::OK
    );
    assert_eq!(w.call("POST", &format!("/complaints/{id}/notes"), Some(&rep), Some(json!({ "body": "  " }))).await.0, StatusCode::BAD_REQUEST);
    let (_, d, _) = w.call("GET", &format!("/complaints/{id}"), Some(&rep), None).await;
    assert_eq!((d["status"].as_str(), d["notes"].as_array().unwrap().len()), (Some("in_progress"), 1), "a reply moves it on");
    assert_eq!(w.call("PUT", &format!("/complaints/{id}"), Some(&rep), Some(json!({ "assign": "me", "status": "resolved" }))).await.0, StatusCode::OK);
    let (_, d, _) = w.call("GET", &format!("/complaints/{id}"), Some(&rep), None).await;
    assert_eq!((d["status"].as_str(), d["assigned_to"].as_str()), (Some("resolved"), Some("rep@x.test")));
    assert_eq!(w.call("PUT", &format!("/complaints/{id}"), Some(&rep), Some(json!({ "status": "banana" }))).await.0, StatusCode::BAD_REQUEST);
    // flooding is limited per number
    for _ in 0..6 {
        let _ = w.public("POST", "/complaints", Some(json!({ "phone": "08099998888", "message": "spam spam" }))).await;
    }
    assert_eq!(w.public("POST", "/complaints", Some(json!({ "phone": "08099998888", "message": "spam spam" }))).await.0, StatusCode::TOO_MANY_REQUESTS);
}

// ---------------------------------------------------------------------------
// Error log
// ---------------------------------------------------------------------------

#[tokio::test]
async fn errors_from_the_system_the_web_app_and_the_hardware_all_land_in_one_log() {
    let Some(w) = world().await else { return };
    let sys = w.staff("sys@x.test", "system_admin").await;
    let adm = w.staff("adm@x.test", "admin").await;
    let rep = w.staff("rep@x.test", "customer_rep").await;

    // 1. SYSTEM: Paystack being down turns a checkout into a 502, which the log middleware records with its cause.
    let (s, _, _) = w.public("POST", "/orders", Some(json!({ "site": "site-1", "plan_id": w.plan, "phone": "08031234567" }))).await;
    assert_eq!(s, StatusCode::BAD_GATEWAY);
    // 2. WEB APP: the browser reports a crash and a failed call.
    let (s, _, _) = w.public("POST", "/client-errors", Some(json!({ "message": "Cannot read properties of undefined (reading 'name')", "kind": "js", "stack": "at Plans.vue:41", "url": "https://wifi.test/", "component": "Plans" }))).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _, _) = w.public("POST", "/client-errors", Some(json!({ "message": "GET /sites/site-1/plans failed: 502", "kind": "api" }))).await;
    assert_eq!(s, StatusCode::OK);
    // 3. HARDWARE: the site agent reports with its own token.
    let (_, tok, _) = w.call("POST", "/node-tokens", Some(&sys), Some(json!({ "site": "site-1", "name": "Site 1 PC" }))).await;
    let nt = tok["token"].as_str().unwrap().to_string();
    let (s, v, _) = w
        .req("POST", "/v1/node/errors", Some(&nt), Some(json!({ "events": [
            { "source": "hardware", "severity": "critical", "code": "hardware.router_unreachable", "message": "REST call to 192.168.88.1 timed out", "details": { "router": "192.168.88.1", "attempts": 3 } },
            { "source": "radius", "severity": "error", "code": "radius.coa_failed", "message": "radclient exit 1", "details": { "mac": "AA:BB:CC:00:11:22" } },
            { "code": "bad code!!", "message": "dropped chars only" },
        ] })))
        .await;
    assert_eq!((s, v["stored"].as_i64()), (StatusCode::OK, Some(3)), "{v}");
    assert_eq!(w.req("POST", "/v1/node/errors", Some("nt_wrongwrongwrongwrongwrongwrong"), Some(json!({ "events": [] }))).await.0, StatusCode::UNAUTHORIZED);

    // Everyone on staff, reps included, can read it.
    for t in [&sys, &adm, &rep] {
        let (s, list, _) = w.call("GET", "/errors", Some(t), None).await;
        assert_eq!(s, StatusCode::OK);
        let sources: Vec<&str> = list.as_array().unwrap().iter().map(|e| e["source"].as_str().unwrap()).collect();
        for want in ["payment", "webapp", "hardware", "radius"] {
            assert!(sources.contains(&want), "missing {want} in {sources:?}");
        }
    }
    let (_, pay, _) = w.call("GET", "/errors?source=payment", Some(&rep), None).await;
    let e = &pay[0];
    assert_eq!(e["code"], "payment.gateway");
    assert!(e["message"].as_str().unwrap().contains("paystack timed out"), "the real cause is kept: {e}");
    assert!(e["hint"].as_str().unwrap().contains("Paystack"), "and a first thing to check");
    let (_, full, _) = w.call("GET", &format!("/errors/{}", e["id"]), Some(&rep), None).await;
    assert_eq!(full["details"]["status"], 502);
    assert_eq!(full["details"]["method"], "POST");
    let (_, hw, _) = w.call("GET", "/errors?source=hardware", Some(&rep), None).await;
    let router = hw.as_array().unwrap().iter().find(|e| e["code"] == "hardware.router_unreachable").expect("router event");
    assert_eq!(router["site"], "Site 1");
    assert_eq!(router["details"]["router"], "192.168.88.1");
    assert!(hw.as_array().unwrap().iter().any(|e| e["code"] == "badcode"), "odd characters are stripped from codes");
    let (_, web, _) = w.call("GET", "/errors?q=undefined", Some(&rep), None).await;
    assert_eq!(web[0]["details"]["component"], "Plans");
    // filters validate
    assert_eq!(w.call("GET", "/errors?source=nonsense", Some(&rep), None).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(w.call("GET", "/errors?severity=critical", Some(&rep), None).await.1.as_array().unwrap().len(), 1);

    // Summary and grouping
    for _ in 0..3 {
        w.public("POST", "/client-errors", Some(json!({ "message": "Failed to load chunk 17", "kind": "js" }))).await;
        w.public("POST", "/client-errors", Some(json!({ "message": "Failed to load chunk 4", "kind": "js" }))).await;
    }
    let (_, sum, _) = w.call("GET", "/errors/summary", Some(&rep), None).await;
    assert!(sum["open_critical"].as_i64().unwrap() >= 1);
    assert!(sum["last_24h_by_source"]["webapp"].as_i64().unwrap() >= 7);
    let top = &sum["top_problems"][0];
    assert_eq!(top["times"], 6, "chunk 17 and chunk 4 are the same problem: {top}");

    // Reps read; admins and above resolve (one, or every repeat of it).
    let id = web[0]["id"].as_i64().unwrap();
    assert_eq!(w.call("POST", &format!("/errors/{id}/resolve"), Some(&rep), Some(json!({}))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(w.call("POST", &format!("/errors/{id}/resolve"), Some(&adm), Some(json!({}))).await.1["resolved"], 1);
    let (_, open, _) = w.call("GET", "/errors?q=undefined", Some(&rep), None).await;
    assert!(open.as_array().unwrap().is_empty());
    let (_, done, _) = w.call("GET", "/errors?q=undefined&state=resolved", Some(&rep), None).await;
    assert_eq!(done.as_array().unwrap().len(), 1);
    let (_, chunk, _) = w.call("GET", "/errors?q=chunk", Some(&rep), None).await;
    let cid = chunk[0]["id"].as_i64().unwrap();
    assert_eq!(
        w.call("POST", &format!("/errors/{cid}/resolve"), Some(&adm), Some(json!({ "all_same": true }))).await.1["resolved"],
        6,
        "chunk 17 and chunk 4 are one problem"
    );

    // spreadsheet export, neutralised against formula injection
    w.public("POST", "/client-errors", Some(json!({ "message": "=HYPERLINK(\"http://evil\")", "kind": "js" }))).await;
    let (s, _, csv) = w.call("GET", "/errors.csv", Some(&rep), None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(csv.starts_with("id,time,source,severity,code,message,hint"));
    assert!(csv.contains("\"'=HYPERLINK"), "{csv}");
}

#[tokio::test]
async fn the_log_is_private_to_each_establishment() {
    let Some(w) = world().await else { return };
    let Some(other) = world().await else { return };
    w.public("POST", "/client-errors", Some(json!({ "message": "only for the first one", "kind": "js" }))).await;
    let t = other.staff("sys@x.test", "system_admin").await;
    let (_, list, _) = other.call("GET", "/errors", Some(&t), None).await;
    assert!(list.as_array().unwrap().iter().all(|e| e["message"] != "only for the first one"));
}

#[tokio::test]
async fn a_silent_site_agent_is_reported_once_and_again_when_it_returns() {
    let Some(w) = world().await else { return };
    let sys = w.staff("sys@x.test", "system_admin").await;
    sqlx::query("INSERT INTO metrics (ts, tenant_id, site_id, metric, value) VALUES (now() - interval '20 minutes', $1, $2, 'agent_up', 1)")
        .bind(w.tenant)
        .bind(w.site_id)
        .execute(&w.admin)
        .await
        .unwrap();
    platform_api::errlog::watch_once(&w.state.pool).await.unwrap();
    platform_api::errlog::watch_once(&w.state.pool).await.unwrap();
    let (_, list, _) = w.call("GET", "/errors?source=hardware", Some(&sys), None).await;
    assert_eq!(list.as_array().unwrap().len(), 1, "one offline event, not one per minute: {list}");
    assert_eq!(list[0]["code"], "hardware.node_offline");
    assert_eq!(list[0]["severity"], "critical");
    sqlx::query("INSERT INTO metrics (ts, tenant_id, site_id, metric, value) VALUES (now(), $1, $2, 'agent_up', 1)")
        .bind(w.tenant)
        .bind(w.site_id)
        .execute(&w.admin)
        .await
        .unwrap();
    platform_api::errlog::watch_once(&w.state.pool).await.unwrap();
    platform_api::errlog::watch_once(&w.state.pool).await.unwrap();
    let (_, list, _) = w.call("GET", "/errors?source=hardware&state=all", Some(&sys), None).await;
    let codes: Vec<&str> = list.as_array().unwrap().iter().map(|e| e["code"].as_str().unwrap()).collect();
    assert_eq!(codes.iter().filter(|c| **c == "hardware.node_recovered").count(), 1, "{codes:?}");
    assert_eq!(codes.iter().filter(|c| **c == "hardware.node_offline").count(), 1);
}

// ---------------------------------------------------------------------------
// Portability: new establishments and branding
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_new_establishment_is_one_command_and_is_fully_separate() {
    let Some(w) = world().await else { return };
    let slug = format!("new-{}", &Uuid::new_v4().simple().to_string()[..8]);
    let id = platform_api::tenants::create_tenant(&w.state.pool, &slug, "Hotel Minna", "lobby", "Lobby").await.unwrap();
    assert!(platform_api::tenants::create_tenant(&w.state.pool, &slug, "Again", "lobby", "Lobby").await.is_err(), "slug is unique");
    assert!(platform_api::tenants::create_tenant(&w.state.pool, "Bad Slug", "x", "lobby", "Lobby").await.is_err());
    let (sites, profile): (i64, i32) =
        sqlx::query_as("SELECT (SELECT count(*) FROM sites WHERE tenant_id = $1), (SELECT down_kbps FROM speed_profiles WHERE tenant_id = $1)")
            .bind(id)
            .fetch_one(&w.admin)
            .await
            .unwrap();
    assert_eq!((sites, profile), (1, 2000), "a site and the default 2 Mbps profile");
    platform_api::admin::create_admin(&w.state, &slug, "owner@hotel.test", PW, "system_admin").await.unwrap();
    let (s, v, _) = w.req("POST", &format!("/v1/t/{slug}/admin/login"), None, Some(json!({ "email": "owner@hotel.test", "password": PW }))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let tok = v["token"].as_str().unwrap().to_string();
    // its staff token means nothing at the first establishment
    assert_eq!(w.call("GET", "/overview", Some(&tok), None).await.0, StatusCode::UNAUTHORIZED);
    // and it sees none of the first one's plans
    let (_, plans, _) = w.req("GET", &format!("/v1/t/{slug}/admin/plans"), Some(&tok), None).await;
    assert!(plans.as_array().unwrap().is_empty());

    // branding: public read, CEO/system admin write, validated
    let (s, b, _) = w.req("GET", &format!("/v1/t/{slug}/brand"), None, None).await;
    assert_eq!((s, b["brand"].as_str(), b["sites"][0]["slug"].as_str()), (StatusCode::OK, Some("Hotel Minna"), Some("lobby")));
    let brand_path = format!("/v1/t/{slug}/admin/brand");
    let put = |body: Value| w.req("PUT", &brand_path, Some(&tok), Some(body));
    let (s, v, _) = put(json!({ "brand": "Hotel Minna Wi-Fi", "supportPhone": "0803 123 4567", "primary": "#1D4ED8", "tagline": "Stay connected", "hotspotHosts": ["10.5.50.1"] })).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(put(json!({ "primary": "blue" })).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(put(json!({ "hotspotHosts": ["a/b"] })).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(put(json!({ "supportPhone": "call me maybe" })).await.0, StatusCode::BAD_REQUEST);
    let (_, b, _) = w.req("GET", &format!("/v1/t/{slug}/brand"), None, None).await;
    assert_eq!(
        (b["brand"].as_str(), b["primary"].as_str(), b["supportPhone"].as_str(), b["hotspotHosts"][0].as_str()),
        (Some("Hotel Minna Wi-Fi"), Some("#1D4ED8"), Some("0803 123 4567"), Some("10.5.50.1"))
    );
    assert_eq!(w.req("GET", "/v1/t/does-not-exist/brand", None, None).await.0, StatusCode::NOT_FOUND);
}
