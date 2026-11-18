//! Creating an order and opening the Paystack payment for it.
//!
//! The order row exists BEFORE the customer pays. Its `reference` is what ties
//! a later webhook back to it, and its `amount_kobo` is what we compare the
//! gateway's amount against. We never trust an amount sent by the browser:
//! the price always comes from the plan row.

use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::codes::new_order_reference;
use crate::db::{audit, load_plan, tenant_id_by_slug, tenant_tx};
use crate::error::{AppError, Result};
use crate::paystack::InitializeRequest;
use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct CreateOrder {
    pub site: String,
    pub plan_id: Uuid,
    pub phone: String,
    #[serde(default)]
    pub email: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CreatedOrder {
    pub reference: String,
    pub authorization_url: String,
    pub amount_kobo: i64,
}

/// `0801 234 5678` and `+234 801 234 5678` both become `+2348012345678`.
/// Returns `None` if the result is not 8 to 15 digits.
pub fn normalise_phone(input: &str) -> Option<String> {
    let digits: String = input.chars().filter(|c| c.is_ascii_digit()).collect();
    let had_plus = input.trim_start().starts_with('+');
    let full = if had_plus {
        digits
    } else if digits.len() == 11 && digits.starts_with('0') {
        format!("234{}", &digits[1..]) // Nigerian national format
    } else {
        digits
    };
    if (8..=15).contains(&full.len()) {
        Some(format!("+{full}"))
    } else {
        None
    }
}

/// Paystack needs an email. Members and guests who give one use it; others get
/// a stable placeholder on the operator's own domain so receipts can still be matched.
pub fn payer_email(given: Option<&str>, phone: &str, fallback_domain: &str) -> String {
    match given.map(str::trim) {
        Some(e) if e.contains('@') && e.len() <= 120 => e.to_string(),
        _ => format!("{}@{}", phone.trim_start_matches('+'), fallback_domain),
    }
}

pub async fn create_order(state: &AppState, tenant_slug: &str, req: CreateOrder) -> Result<CreatedOrder> {
    let phone = normalise_phone(&req.phone).ok_or_else(|| AppError::BadRequest("enter a valid phone number".into()))?;
    let tenant = tenant_id_by_slug(&state.pool, tenant_slug).await?;

    // 1. Create the order inside the tenant's transaction.
    let mut tx = tenant_tx(&state.pool, tenant).await?;
    let site_id: Uuid = sqlx::query("SELECT id FROM sites WHERE slug = $1 AND status = 'active'")
        .bind(&req.site)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(AppError::NotFound)?
        .try_get("id")?;

    let plan = load_plan(&mut tx, req.plan_id).await?;
    let is_active: bool = sqlx::query("SELECT is_active FROM plans WHERE id = $1").bind(req.plan_id).fetch_one(&mut *tx).await?.try_get("is_active")?;
    if !is_active {
        return Err(AppError::NotFound);
    }
    if plan.price_kobo <= 0 {
        return Err(AppError::BadRequest("this plan cannot be bought online".into()));
    }

    let reference = new_order_reference();
    let order_id: Uuid = sqlx::query(
        "INSERT INTO orders (tenant_id, site_id, plan_id, reference, amount_kobo, phone, status)
         VALUES ($1, $2, $3, $4, $5, $6, 'created') RETURNING id",
    )
    .bind(tenant)
    .bind(site_id)
    .bind(req.plan_id)
    .bind(&reference)
    .bind(plan.price_kobo)
    .bind(&phone)
    .fetch_one(&mut *tx)
    .await?
    .try_get("id")?;

    let t = sqlx::query("SELECT payment_mode, paystack_subaccount FROM tenants WHERE id = $1").bind(tenant).fetch_one(&mut *tx).await?;
    let mode: String = t.try_get("payment_mode")?;
    let subaccount: Option<String> = t.try_get("paystack_subaccount")?;
    let secret = state.load_paystack_secret(&mut tx, tenant).await?;

    audit(
        &mut tx,
        tenant,
        "guest",
        Some(&phone),
        "order.created",
        Some(("order", &order_id.to_string())),
        serde_json::json!({ "plan": plan.name, "amount_kobo": plan.price_kobo, "site": req.site }),
    )
    .await?;
    tx.commit().await?;

    // 2. Open the payment with Paystack. No database transaction is held open
    //    during this network call.
    let email = payer_email(req.email.as_deref(), &phone, &state.config.fallback_email_domain);
    let callback = format!("{}/order/{}", state.config.public_base_url, reference);
    let init = state
        .gateway
        .initialize(InitializeRequest {
            secret: &secret,
            reference: &reference,
            amount_kobo: plan.price_kobo,
            currency: "NGN",
            email: &email,
            callback_url: Some(&callback),
            subaccount: if mode == "subaccount" { subaccount.as_deref() } else { None },
            metadata: serde_json::json!({ "order_id": order_id, "plan": plan.name, "site": req.site }),
        })
        .await;

    let mut tx = tenant_tx(&state.pool, tenant).await?;
    match init {
        Ok(i) => {
            sqlx::query("UPDATE orders SET status = 'pending' WHERE id = $1 AND status = 'created'").bind(order_id).execute(&mut *tx).await?;
            tx.commit().await?;
            Ok(CreatedOrder { reference, authorization_url: i.authorization_url, amount_kobo: plan.price_kobo })
        }
        Err(e) => {
            sqlx::query("UPDATE orders SET status = 'failed' WHERE id = $1 AND status = 'created'").bind(order_id).execute(&mut *tx).await?;
            audit(
                &mut tx,
                tenant,
                "system",
                None,
                "order.gateway_failed",
                Some(("order", &order_id.to_string())),
                serde_json::json!({ "error": e.to_string() }),
            )
            .await?;
            tx.commit().await?;
            Err(e)
        }
    }
}

#[derive(Debug, Serialize)]
pub struct OrderStatus {
    pub status: String,
    /// Shown once the order is provisioned, and only to someone who knows the
    /// unguessable order reference.
    pub access_code: Option<String>,
    pub plan: String,
}

pub async fn order_status(state: &AppState, tenant_slug: &str, reference: &str) -> Result<OrderStatus> {
    let tenant = tenant_id_by_slug(&state.pool, tenant_slug).await?;
    let mut tx = tenant_tx(&state.pool, tenant).await?;
    let row = sqlx::query(
        "SELECT o.status::text AS status, COALESCE(p.name, 'Wallet top-up') AS plan, c.code AS code
           FROM orders o
           LEFT JOIN plans p ON p.id = o.plan_id
           LEFT JOIN access_codes c ON c.id = o.access_code_id
          WHERE o.reference = $1",
    )
    .bind(reference)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    tx.commit().await?;
    Ok(OrderStatus { status: row.try_get("status")?, access_code: row.try_get("code")?, plan: row.try_get("plan")? })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phones_are_normalised() {
        assert_eq!(normalise_phone("0801 234 5678").as_deref(), Some("+2348012345678"));
        assert_eq!(normalise_phone("+234 801-234-5678").as_deref(), Some("+2348012345678"));
        assert_eq!(normalise_phone("2348012345678").as_deref(), Some("+2348012345678"));
        assert_eq!(normalise_phone("12345"), None);
        assert_eq!(normalise_phone("not a number"), None);
        assert_eq!(normalise_phone("+1 415 555 0100").as_deref(), Some("+14155550100"));
    }

    #[test]
    fn payer_email_uses_given_or_placeholder() {
        assert_eq!(payer_email(Some("a@b.com"), "+2348012345678", "wifi.example"), "a@b.com");
        assert_eq!(payer_email(Some("nope"), "+2348012345678", "wifi.example"), "2348012345678@wifi.example");
        assert_eq!(payer_email(None, "+2348012345678", "wifi.example"), "2348012345678@wifi.example");
    }
}
