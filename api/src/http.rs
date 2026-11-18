//! HTTP routes.
//!
//! | Method | Path                                        | Who calls it          |
//! |--------|---------------------------------------------|-----------------------|
//! | GET    | `/healthz`                                  | load balancer         |
//! | GET    | `/v1/t/:tenant/sites/:site/plans`           | portal                |
//! | POST   | `/v1/t/:tenant/orders`                      | portal                |
//! | GET    | `/v1/t/:tenant/orders/:reference`           | portal (polls status) |
//! | POST   | `/v1/t/:tenant/webhooks/paystack`           | Paystack              |

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use uuid::Uuid;

use crate::admin;
use crate::db::{audit, list_active_plans, tenant_id_by_slug, tenant_tx};
use crate::error::{AppError, Result};
use crate::fulfil::{fulfil_payment, Fulfilment};
use crate::member;
use crate::orders::{create_order, order_status, CreateOrder, CreatedOrder, OrderStatus};
use crate::paystack::{signature_is_valid, WebhookEvent};
use crate::radius::{self, AccountingReply, AccountingRequest, AuthorizeReply, AuthorizeRequest, ConnectRequest, Ticket};
use crate::refunds;
use crate::AppState;
use crate::{complaints, errapi, staff, tenants, vouchers};

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/t/:tenant/sites/:site/plans", get(plans))
        .route("/v1/t/:tenant/orders", post(new_order))
        .route("/v1/t/:tenant/orders/:reference", get(get_order))
        .route("/v1/t/:tenant/webhooks/paystack", post(paystack_webhook))
        .route("/v1/t/:tenant/connect", post(connect_guest))
        .route("/v1/t/:tenant/members/register", post(m_register))
        .route("/v1/t/:tenant/members/login", post(m_login))
        .route("/v1/t/:tenant/members/logout", post(m_logout))
        .route("/v1/t/:tenant/members/otp/request", post(m_otp_request))
        .route("/v1/t/:tenant/members/otp/verify", post(m_otp_verify))
        .route("/v1/t/:tenant/members/me", get(m_me))
        .route("/v1/t/:tenant/members/claim", post(m_claim))
        .route("/v1/t/:tenant/members/connect", post(m_connect))
        .route("/v1/t/:tenant/members/wallet", get(m_wallet))
        .route("/v1/t/:tenant/members/wallet/topup", post(m_topup))
        .route("/v1/t/:tenant/members/wallet/buy", post(m_buy))
        .route("/v1/t/:tenant/members/devices", get(m_devices))
        .route("/v1/t/:tenant/members/devices/swap", post(m_swap))
        .route("/v1/t/:tenant/admin/login", post(a_login))
        .route("/v1/t/:tenant/admin/login/verify", post(a_login_verify))
        .route("/v1/t/:tenant/admin/logout", post(a_logout))
        .route("/v1/t/:tenant/admin/me", get(a_me))
        .route("/v1/t/:tenant/admin/password", axum::routing::put(a_password))
        .route("/v1/t/:tenant/admin/2fa/start", post(a_mfa_start))
        .route("/v1/t/:tenant/admin/2fa/confirm", post(a_mfa_confirm))
        .route("/v1/t/:tenant/admin/users", get(a_users).post(a_user_create))
        .route("/v1/t/:tenant/admin/users/:id", axum::routing::put(a_user_update).delete(a_user_delete))
        .route("/v1/t/:tenant/admin/users/:id/reset-password", post(a_user_reset_password))
        .route("/v1/t/:tenant/admin/users/:id/reset-2fa", post(a_user_reset_mfa))
        .route("/v1/t/:tenant/admin/users/:id/activity", get(a_user_activity))
        .route("/v1/t/:tenant/admin/vouchers", get(a_vouchers))
        .route("/v1/t/:tenant/admin/vouchers/manual", post(a_voucher_manual))
        .route("/v1/t/:tenant/admin/vouchers/special", post(a_voucher_special))
        .route("/v1/t/:tenant/admin/vouchers/:id/revoke", post(a_voucher_revoke))
        .route("/v1/t/:tenant/complaints", post(complaint_submit))
        .route("/v1/t/:tenant/admin/complaints", get(a_complaints))
        .route("/v1/t/:tenant/admin/complaints/:id", get(a_complaint).put(a_complaint_update))
        .route("/v1/t/:tenant/admin/complaints/:id/notes", post(a_complaint_note))
        .route("/v1/t/:tenant/admin/errors", get(a_errors))
        .route("/v1/t/:tenant/admin/errors.csv", get(a_errors_csv))
        .route("/v1/t/:tenant/admin/errors/summary", get(a_errors_summary))
        .route("/v1/t/:tenant/admin/errors/:id", get(a_error))
        .route("/v1/t/:tenant/admin/errors/:id/resolve", post(a_error_resolve))
        .route("/v1/t/:tenant/client-errors", post(client_error))
        .route("/v1/t/:tenant/brand", get(brand))
        .route("/v1/t/:tenant/admin/brand", axum::routing::put(a_brand))
        .route("/v1/node/errors", post(node_errors))
        .route("/v1/t/:tenant/admin/overview", get(a_overview))
        .route("/v1/t/:tenant/admin/analytics/:what", get(a_analytics))
        .route("/v1/t/:tenant/admin/customers", get(a_customers))
        .route("/v1/t/:tenant/admin/customers/:id", get(a_customer))
        .route("/v1/t/:tenant/admin/plans", get(a_plans).post(a_plan_create))
        .route("/v1/t/:tenant/admin/plans/:id", axum::routing::put(a_plan_update))
        .route("/v1/t/:tenant/admin/plans/:id/active", axum::routing::put(a_plan_active))
        .route("/v1/t/:tenant/admin/speed-profiles", get(a_profiles).post(a_profile_create))
        .route("/v1/t/:tenant/admin/network", get(a_network))
        .route("/v1/t/:tenant/admin/node-tokens", post(a_token_create))
        .route("/v1/t/:tenant/admin/node-tokens/:id/revoke", post(a_token_revoke))
        .route("/v1/t/:tenant/admin/security", get(a_security))
        .route("/v1/t/:tenant/admin/audit", get(a_audit))
        .route("/v1/t/:tenant/admin/audit/verify", get(a_audit_verify))
        .route("/v1/t/:tenant/admin/audit.csv", get(a_audit_csv))
        .route("/v1/t/:tenant/admin/secrets/paystack", axum::routing::put(a_paystack_key))
        .route("/v1/node/metrics", post(node_metrics))
        .route("/v1/t/:tenant/orders/:reference/refund", get(refund_status).post(refund_request))
        .route("/v1/t/:tenant/admin/orders", get(a_orders))
        .route("/v1/t/:tenant/admin/orders/:id/refund", post(a_refund_execute))
        .route("/v1/t/:tenant/admin/refunds", get(a_refunds))
        .route("/v1/t/:tenant/admin/refunds/:id/decline", post(a_refund_decline))
        .route("/v1/node/radius/authorize", post(node_authorize))
        .route("/v1/node/radius/accounting", post(node_accounting))
        // Webhook and order bodies are small. Refuse anything large before reading it.
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(tower_http::catch_panic::CatchPanicLayer::custom(crate::errlog::on_panic))
        .layer(axum::middleware::from_fn_with_state(state.clone(), crate::errlog::capture))
        .with_state(state)
}

#[derive(Serialize)]
struct PlanView {
    id: Uuid,
    name: String,
    kind: &'static str,
    price_kobo: i64,
    active_seconds: Option<u32>,
    data_bytes: Option<u64>,
    valid_seconds: u32,
    device_limit: u8,
    down_kbps: u32,
    up_kbps: u32,
}

async fn plans(State(st): State<AppState>, Path((tenant, site)): Path<(String, String)>) -> Result<Json<Vec<PlanView>>> {
    let tenant_id = tenant_id_by_slug(&st.pool, &tenant).await?;
    let mut tx = tenant_tx(&st.pool, tenant_id).await?;
    // The site must exist and be active, so a typo gives 404 rather than an empty shop.
    sqlx::query("SELECT 1 AS ok FROM sites WHERE slug = $1 AND status = 'active'").bind(&site).fetch_optional(&mut *tx).await?.ok_or(AppError::NotFound)?;
    let rows = list_active_plans(&mut tx).await?;
    tx.commit().await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| PlanView {
                id: r.id,
                name: r.name,
                kind: match r.plan.kind {
                    entitlement::PlanKind::Time => "time",
                    entitlement::PlanKind::Data => "data",
                    entitlement::PlanKind::Hybrid => "hybrid",
                },
                price_kobo: r.price_kobo,
                active_seconds: r.plan.active_seconds,
                data_bytes: r.plan.data_bytes,
                valid_seconds: r.plan.valid_seconds,
                device_limit: r.plan.device_limit,
                down_kbps: r.plan.speed.down_kbps,
                up_kbps: r.plan.speed.up_kbps,
            })
            .collect(),
    ))
}

async fn new_order(State(st): State<AppState>, Path(tenant): Path<String>, Json(body): Json<CreateOrder>) -> Result<(StatusCode, Json<CreatedOrder>)> {
    Ok((StatusCode::CREATED, Json(create_order(&st, &tenant, body).await?)))
}

async fn get_order(State(st): State<AppState>, Path((tenant, reference)): Path<(String, String)>) -> Result<Json<OrderStatus>> {
    Ok(Json(order_status(&st, &tenant, &reference).await?))
}

/// Paystack calls this. We answer 200 only when the event is handled (or safely
/// ignorable), so Paystack keeps retrying while we are unable to verify it.
async fn paystack_webhook(State(st): State<AppState>, Path(tenant_slug): Path<String>, headers: HeaderMap, body: Bytes) -> Result<Json<Fulfilment>> {
    let tenant = tenant_id_by_slug(&st.pool, &tenant_slug).await?;

    let mut tx = tenant_tx(&st.pool, tenant).await?;
    let secret = st.load_paystack_secret(&mut tx, tenant).await?;

    // 1. Signature over the RAW bytes, before anything is parsed or trusted.
    let sig = headers.get("x-paystack-signature").and_then(|v| v.to_str().ok()).unwrap_or("");
    if !signature_is_valid(&secret, &body, sig) {
        audit(&mut tx, tenant, "system", None, "payment.webhook_rejected", None, serde_json::json!({ "reason": "bad_signature", "bytes": body.len() })).await?;
        tx.commit().await?;
        return Err(AppError::Unauthorised);
    }
    tx.commit().await?;

    // 2. Only successful charges matter. Everything else is acknowledged and dropped.
    let event = WebhookEvent::parse(&body)?;
    if event.event != "charge.success" {
        return Ok(Json(Fulfilment::NotSuccessful { status: event.event }));
    }
    let reference = event.data.reference.ok_or_else(|| AppError::BadRequest("event has no reference".into()))?;

    // 3. Never trust the webhook's numbers. Ask Paystack.
    let verified = st.gateway.verify(&secret, &reference).await?;
    if verified.reference != reference {
        return Err(AppError::BadRequest("gateway answered for a different reference".into()));
    }

    // 4. Grant access, once.
    Ok(Json(fulfil_payment(&st.pool, tenant, &verified).await?))
}

/// Guest sign-in: access code in, hotspot ticket out.
async fn connect_guest(State(st): State<AppState>, Path(tenant): Path<String>, headers: HeaderMap, Json(body): Json<ConnectRequest>) -> Result<Json<Ticket>> {
    let ip = crate::auth::client_ip(&headers, st.config.trust_proxy);
    Ok(Json(radius::connect_with_code(&st, &tenant, &headers, ip.as_deref(), body).await?))
}

/// The site agent asks whether a ticket may connect.
async fn node_authorize(State(st): State<AppState>, headers: HeaderMap, Json(body): Json<AuthorizeRequest>) -> Result<Json<AuthorizeReply>> {
    let node = radius::authenticate_node(&st, &headers).await?;
    Ok(Json(radius::authorize(&st, node, body).await?))
}

/// The site agent reports usage and learns whether to keep the session.
async fn node_accounting(State(st): State<AppState>, headers: HeaderMap, Json(body): Json<AccountingRequest>) -> Result<Json<AccountingReply>> {
    let node = radius::authenticate_node(&st, &headers).await?;
    Ok(Json(radius::accounting(&st, node, body).await?))
}

// ---- Members ---------------------------------------------------------------

type Tenant = Path<String>;

async fn m_register(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Json(b): Json<member::Register>) -> Result<Json<serde_json::Value>> {
    Ok(Json(member::register(&st, &t, &h, b).await?))
}
async fn m_login(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Json(b): Json<member::Login>) -> Result<Json<serde_json::Value>> {
    Ok(Json(member::login(&st, &t, &h, b).await?))
}
async fn m_logout(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap) -> Result<Json<serde_json::Value>> {
    Ok(Json(member::logout(&st, &t, &h).await?))
}
async fn m_otp_request(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Json(b): Json<member::OtpRequest>) -> Result<Json<serde_json::Value>> {
    Ok(Json(member::otp_request(&st, &t, &h, b).await?))
}
async fn m_otp_verify(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Json(b): Json<member::OtpVerify>) -> Result<Json<serde_json::Value>> {
    Ok(Json(member::otp_verify(&st, &t, &h, b).await?))
}
async fn m_me(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap) -> Result<Json<serde_json::Value>> {
    Ok(Json(member::me(&st, &t, &h).await?))
}
async fn m_claim(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Json(b): Json<member::Claim>) -> Result<Json<serde_json::Value>> {
    Ok(Json(member::claim_code(&st, &t, &h, b).await?))
}
async fn m_connect(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Json(b): Json<member::MemberConnect>) -> Result<Json<Ticket>> {
    Ok(Json(member::connect(&st, &t, &h, b).await?))
}
async fn m_wallet(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap) -> Result<Json<serde_json::Value>> {
    Ok(Json(member::wallet(&st, &t, &h).await?))
}
async fn m_topup(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Json(b): Json<member::TopUp>) -> Result<Json<serde_json::Value>> {
    Ok(Json(member::topup(&st, &t, &h, b).await?))
}
async fn m_buy(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Json(b): Json<member::BuyWithWallet>) -> Result<Json<serde_json::Value>> {
    Ok(Json(member::buy_with_wallet(&st, &t, &h, b).await?))
}
async fn m_devices(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap) -> Result<Json<serde_json::Value>> {
    Ok(Json(member::devices(&st, &t, &h).await?))
}
async fn m_swap(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Json(b): Json<member::Swap>) -> Result<Json<serde_json::Value>> {
    Ok(Json(member::swap_device(&st, &t, &h, b).await?))
}

// ---- Admin -----------------------------------------------------------------

type J = Result<Json<serde_json::Value>>;

#[derive(serde::Deserialize, Default)]
struct Q {
    range: Option<String>,
    q: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
    action: Option<String>,
    before: Option<i64>,
    from: Option<i64>,
    status: Option<String>,
    kind: Option<String>,
    actor: Option<String>,
}

async fn a_login(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Json(b): Json<staff::Login>) -> J {
    Ok(Json(staff::login(&st, &t, &h, b).await?))
}
async fn a_login_verify(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Json(b): Json<staff::Verify>) -> J {
    Ok(Json(staff::login_verify(&st, &t, &h, b).await?))
}
async fn a_logout(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap) -> J {
    Ok(Json(admin::logout(&st, &t, &h).await?))
}
async fn a_me(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap) -> J {
    Ok(Json(staff::me(&st, &t, &h).await?))
}
async fn a_password(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Json(b): Json<staff::ChangePassword>) -> J {
    Ok(Json(staff::change_password(&st, &t, &h, b).await?))
}
async fn a_mfa_start(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Json(b): Json<staff::StartMfa>) -> J {
    Ok(Json(staff::mfa_start(&st, &t, &h, b).await?))
}
async fn a_mfa_confirm(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Json(b): Json<staff::Verify>) -> J {
    Ok(Json(staff::mfa_confirm(&st, &t, &h, b).await?))
}
async fn a_users(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Query(q): Query<Q>) -> J {
    Ok(Json(staff::list_users(&st, &t, &h, q.status.as_deref() == Some("all")).await?))
}
async fn a_user_create(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Json(b): Json<staff::NewUser>) -> J {
    Ok(Json(staff::create_user(&st, &t, &h, b).await?))
}
async fn a_user_update(State(st): State<AppState>, Path((t, id)): Path<(String, Uuid)>, h: HeaderMap, Json(b): Json<staff::UserPatch>) -> J {
    Ok(Json(staff::update_user(&st, &t, &h, id, b).await?))
}
async fn a_user_delete(State(st): State<AppState>, Path((t, id)): Path<(String, Uuid)>, h: HeaderMap) -> J {
    Ok(Json(staff::delete_user(&st, &t, &h, id).await?))
}
async fn a_user_reset_password(State(st): State<AppState>, Path((t, id)): Path<(String, Uuid)>, h: HeaderMap, Json(b): Json<staff::ResetPassword>) -> J {
    Ok(Json(staff::reset_password(&st, &t, &h, id, b).await?))
}
async fn a_user_reset_mfa(State(st): State<AppState>, Path((t, id)): Path<(String, Uuid)>, h: HeaderMap) -> J {
    Ok(Json(staff::reset_mfa(&st, &t, &h, id).await?))
}
async fn a_user_activity(State(st): State<AppState>, Path((t, id)): Path<(String, Uuid)>, h: HeaderMap, Query(q): Query<Q>) -> J {
    Ok(Json(staff::user_activity(&st, &t, &h, id, q.before, q.limit).await?))
}
async fn a_vouchers(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Query(q): Query<Q>) -> J {
    Ok(Json(vouchers::list(&st, &t, &h, q.kind.as_deref(), q.q.as_deref(), q.limit).await?))
}
async fn a_voucher_manual(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Json(b): Json<vouchers::ManualSale>) -> J {
    Ok(Json(vouchers::manual_sale(&st, &t, &h, b).await?))
}
async fn a_voucher_special(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Json(b): Json<vouchers::SpecialVoucher>) -> J {
    Ok(Json(vouchers::special_voucher(&st, &t, &h, b).await?))
}
async fn a_voucher_revoke(State(st): State<AppState>, Path((t, id)): Path<(String, Uuid)>, h: HeaderMap) -> J {
    Ok(Json(vouchers::revoke(&st, &t, &h, id).await?))
}
async fn complaint_submit(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Json(b): Json<complaints::NewComplaint>) -> J {
    Ok(Json(complaints::submit(&st, &t, &h, b).await?))
}
async fn a_complaints(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Query(q): Query<Q>) -> J {
    Ok(Json(complaints::list(&st, &t, &h, q.status.as_deref(), q.q.as_deref()).await?))
}
async fn a_complaint(State(st): State<AppState>, Path((t, id)): Path<(String, Uuid)>, h: HeaderMap) -> J {
    Ok(Json(complaints::detail(&st, &t, &h, id).await?))
}
async fn a_complaint_update(State(st): State<AppState>, Path((t, id)): Path<(String, Uuid)>, h: HeaderMap, Json(b): Json<complaints::Update>) -> J {
    Ok(Json(complaints::update(&st, &t, &h, id, b).await?))
}
async fn a_complaint_note(State(st): State<AppState>, Path((t, id)): Path<(String, Uuid)>, h: HeaderMap, Json(b): Json<complaints::Note>) -> J {
    Ok(Json(complaints::add_note(&st, &t, &h, id, b).await?))
}
async fn a_errors(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Query(f): Query<errapi::Filter>) -> J {
    Ok(Json(errapi::list(&st, &t, &h, f).await?))
}
async fn a_errors_csv(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Query(f): Query<errapi::Filter>) -> Result<axum::response::Response> {
    errapi::csv(&st, &t, &h, f).await
}
async fn a_errors_summary(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap) -> J {
    Ok(Json(errapi::summary(&st, &t, &h).await?))
}
async fn a_error(State(st): State<AppState>, Path((t, id)): Path<(String, i64)>, h: HeaderMap) -> J {
    Ok(Json(errapi::detail(&st, &t, &h, id).await?))
}
async fn a_error_resolve(State(st): State<AppState>, Path((t, id)): Path<(String, i64)>, h: HeaderMap, body: Option<Json<errapi::Resolve>>) -> J {
    Ok(Json(errapi::resolve(&st, &t, &h, id, body.map(|b| b.0).unwrap_or_default()).await?))
}
async fn client_error(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Json(b): Json<errapi::ClientError>) -> J {
    Ok(Json(errapi::client_error(&st, &t, &h, b).await?))
}
async fn node_errors(State(st): State<AppState>, h: HeaderMap, Json(b): Json<errapi::NodeErrors>) -> J {
    Ok(Json(errapi::node_errors(&st, &h, b).await?))
}
async fn brand(State(st): State<AppState>, Path(t): Tenant) -> J {
    Ok(Json(tenants::brand(&st, &t).await?))
}
async fn a_brand(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Json(b): Json<tenants::BrandIn>) -> J {
    Ok(Json(tenants::set_brand(&st, &t, &h, b).await?))
}
async fn a_overview(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Query(q): Query<Q>) -> J {
    Ok(Json(admin::overview(&st, &t, &h, q.range.as_deref()).await?))
}
async fn a_analytics(State(st): State<AppState>, Path((t, what)): Path<(String, String)>, h: HeaderMap, Query(q): Query<Q>) -> J {
    Ok(Json(admin::analytics(&st, &t, &h, &what, q.range.as_deref()).await?))
}
async fn a_customers(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Query(q): Query<Q>) -> J {
    Ok(Json(admin::customers(&st, &t, &h, q.q.as_deref(), q.limit, q.offset).await?))
}
async fn a_customer(State(st): State<AppState>, Path((t, id)): Path<(String, Uuid)>, h: HeaderMap) -> J {
    Ok(Json(admin::customer_detail(&st, &t, &h, id).await?))
}
async fn a_plans(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap) -> J {
    Ok(Json(admin::list_plans(&st, &t, &h).await?))
}
async fn a_plan_create(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Json(b): Json<admin::PlanIn>) -> J {
    Ok(Json(admin::save_plan(&st, &t, &h, None, b).await?))
}
async fn a_plan_update(State(st): State<AppState>, Path((t, id)): Path<(String, Uuid)>, h: HeaderMap, Json(b): Json<admin::PlanIn>) -> J {
    Ok(Json(admin::save_plan(&st, &t, &h, Some(id), b).await?))
}
#[derive(serde::Deserialize)]
struct Active {
    active: bool,
}
async fn a_plan_active(State(st): State<AppState>, Path((t, id)): Path<(String, Uuid)>, h: HeaderMap, Json(b): Json<Active>) -> J {
    Ok(Json(admin::set_plan_active(&st, &t, &h, id, b.active).await?))
}
async fn a_profiles(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap) -> J {
    Ok(Json(admin::list_profiles(&st, &t, &h).await?))
}
async fn a_profile_create(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Json(b): Json<admin::ProfileIn>) -> J {
    Ok(Json(admin::create_profile(&st, &t, &h, b).await?))
}
async fn a_network(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap) -> J {
    Ok(Json(admin::network(&st, &t, &h).await?))
}
async fn a_token_create(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Json(b): Json<admin::NewToken>) -> J {
    Ok(Json(admin::create_node_token(&st, &t, &h, b).await?))
}
async fn a_token_revoke(State(st): State<AppState>, Path((t, id)): Path<(String, Uuid)>, h: HeaderMap) -> J {
    Ok(Json(admin::revoke_node_token(&st, &t, &h, id).await?))
}
async fn a_security(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap) -> J {
    Ok(Json(admin::security(&st, &t, &h).await?))
}
async fn a_audit(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Query(q): Query<Q>) -> J {
    Ok(Json(admin::audit_list(&st, &t, &h, q.action.as_deref(), q.actor.as_deref(), q.before, q.limit).await?))
}
async fn a_audit_verify(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap) -> J {
    Ok(Json(admin::audit_verify(&st, &t, &h).await?))
}
async fn a_audit_csv(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Query(q): Query<Q>) -> Result<axum::response::Response> {
    admin::audit_csv(&st, &t, &h, q.from).await
}
async fn a_paystack_key(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Json(b): Json<admin::PaystackKey>) -> J {
    Ok(Json(admin::set_paystack_key(&st, &t, &h, b).await?))
}
async fn node_metrics(State(st): State<AppState>, h: HeaderMap, Json(b): Json<admin::MetricsIn>) -> J {
    Ok(Json(admin::node_metrics(&st, &h, b).await?))
}

// ---- Refunds ---------------------------------------------------------------

async fn refund_request(State(st): State<AppState>, Path((t, r)): Path<(String, String)>, h: HeaderMap, Json(b): Json<refunds::RefundAsk>) -> J {
    Ok(Json(refunds::request(&st, &t, &h, &r, b).await?))
}
async fn refund_status(State(st): State<AppState>, Path((t, r)): Path<(String, String)>) -> J {
    Ok(Json(refunds::request_status(&st, &t, &r).await?))
}
async fn a_orders(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Query(q): Query<Q>) -> J {
    Ok(Json(refunds::list_orders(&st, &t, &h, q.q.as_deref(), q.status.as_deref(), q.limit, q.offset).await?))
}
async fn a_refunds(State(st): State<AppState>, Path(t): Tenant, h: HeaderMap, Query(q): Query<Q>) -> J {
    Ok(Json(refunds::list(&st, &t, &h, q.status.as_deref()).await?))
}
async fn a_refund_execute(State(st): State<AppState>, Path((t, id)): Path<(String, Uuid)>, h: HeaderMap, Json(b): Json<refunds::Decision>) -> J {
    Ok(Json(refunds::execute(&st, &t, &h, id, b).await?))
}
async fn a_refund_decline(State(st): State<AppState>, Path((t, id)): Path<(String, Uuid)>, h: HeaderMap, Json(b): Json<refunds::Decision>) -> J {
    Ok(Json(refunds::decline(&st, &t, &h, id, b).await?))
}
