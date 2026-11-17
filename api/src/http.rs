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
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use uuid::Uuid;

use crate::db::{audit, list_active_plans, tenant_id_by_slug, tenant_tx};
use crate::error::{AppError, Result};
use crate::fulfil::{fulfil_payment, Fulfilment};
use crate::orders::{create_order, order_status, CreateOrder, CreatedOrder, OrderStatus};
use crate::paystack::{signature_is_valid, WebhookEvent};
use crate::radius::{self, AccountingReply, AccountingRequest, AuthorizeReply, AuthorizeRequest, ConnectRequest, Ticket};
use crate::AppState;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/t/:tenant/sites/:site/plans", get(plans))
        .route("/v1/t/:tenant/orders", post(new_order))
        .route("/v1/t/:tenant/orders/:reference", get(get_order))
        .route("/v1/t/:tenant/webhooks/paystack", post(paystack_webhook))
        .route("/v1/t/:tenant/connect", post(connect_guest))
        .route("/v1/node/radius/authorize", post(node_authorize))
        .route("/v1/node/radius/accounting", post(node_accounting))
        // Webhook and order bodies are small. Refuse anything large before reading it.
        .layer(DefaultBodyLimit::max(64 * 1024))
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
    sqlx::query("SELECT 1 AS ok FROM sites WHERE slug = $1 AND status = 'active'")
        .bind(&site)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(AppError::NotFound)?;
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

async fn new_order(
    State(st): State<AppState>,
    Path(tenant): Path<String>,
    Json(body): Json<CreateOrder>,
) -> Result<(StatusCode, Json<CreatedOrder>)> {
    Ok((StatusCode::CREATED, Json(create_order(&st, &tenant, body).await?)))
}

async fn get_order(State(st): State<AppState>, Path((tenant, reference)): Path<(String, String)>) -> Result<Json<OrderStatus>> {
    Ok(Json(order_status(&st, &tenant, &reference).await?))
}

/// Paystack calls this. We answer 200 only when the event is handled (or safely
/// ignorable), so Paystack keeps retrying while we are unable to verify it.
async fn paystack_webhook(
    State(st): State<AppState>,
    Path(tenant_slug): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Fulfilment>> {
    let tenant = tenant_id_by_slug(&st.pool, &tenant_slug).await?;

    let mut tx = tenant_tx(&st.pool, tenant).await?;
    let secret = st.load_paystack_secret(&mut tx, tenant).await?;

    // 1. Signature over the RAW bytes, before anything is parsed or trusted.
    let sig = headers.get("x-paystack-signature").and_then(|v| v.to_str().ok()).unwrap_or("");
    if !signature_is_valid(&secret, &body, sig) {
        audit(
            &mut tx,
            tenant,
            "system",
            None,
            "payment.webhook_rejected",
            None,
            serde_json::json!({ "reason": "bad_signature", "bytes": body.len() }),
        )
        .await?;
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
async fn connect_guest(
    State(st): State<AppState>,
    Path(tenant): Path<String>,
    headers: HeaderMap,
    Json(body): Json<ConnectRequest>,
) -> Result<Json<Ticket>> {
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
