//! Turning a verified payment into something the customer owns.
//!
//! Everything happens in ONE database transaction, so a crash can never leave
//! a payment recorded without its entitlement, or the reverse:
//!
//! 1. Lock the order row (`FOR UPDATE`) so two webhooks for the same order
//!    queue up instead of racing.
//! 2. If the order is already provisioned, stop: the webhook was a repeat.
//! 3. Compare the gateway's amount and currency with OUR order. A mismatch is
//!    never provisioned; the order is flagged for refund and an audit entry
//!    is written.
//! 4. Record the payment (unique per gateway transaction id, a second safety net).
//! 5. Guests get a new access code; members use their account.
//! 6. Freeze the plan into `plan_snapshot`, grant the entitlement through the
//!    engine, mark the order provisioned, and write the audit entry.

use serde::Serialize;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::codes::new_access_code;
use crate::db::{audit, load_plan, tenant_tx};
use crate::entitlements_db::{grant, Subject};
use crate::error::{AppError, Result};
use crate::paystack::VerifiedTransaction;

/// What happened to a payment notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Fulfilment {
    /// Access granted.
    Provisioned { entitlement_id: Uuid, access_code: Option<String> },
    /// A wallet top-up was credited.
    WalletCredited { amount_kobo: i64 },
    /// We had already handled this order. Nothing changed.
    AlreadyProcessed,
    /// No order with this reference for this tenant.
    UnknownReference,
    /// The gateway says the transaction did not succeed.
    NotSuccessful { status: String },
    /// Amount or currency differs from the order. Flagged for refund, not provisioned.
    AmountMismatch,
    /// The order is in a state that cannot be fulfilled (for example cancelled).
    OrderNotPayable { status: String },
}

/// Fulfil one verified transaction for `tenant`. Safe to call any number of
/// times with the same transaction: only the first call grants access.
pub async fn fulfil_payment(pool: &PgPool, tenant: Uuid, tx_data: &VerifiedTransaction) -> Result<Fulfilment> {
    let mut tx = tenant_tx(pool, tenant).await?;

    let order = sqlx::query(
        "SELECT id, plan_id, status::text AS status, amount_kobo, currency::text AS currency,
                account_id, phone, site_id, purpose
           FROM orders WHERE reference = $1 FOR UPDATE",
    )
    .bind(&tx_data.reference)
    .fetch_optional(&mut *tx)
    .await?;

    let Some(order) = order else {
        audit(
            &mut tx,
            tenant,
            "paystack",
            None,
            "payment.unknown_reference",
            Some(("payment", &tx_data.id)),
            serde_json::json!({ "reference": tx_data.reference, "amount_kobo": tx_data.amount_kobo }),
        )
        .await?;
        tx.commit().await?;
        return Ok(Fulfilment::UnknownReference);
    };

    let order_id: Uuid = order.try_get("id")?;
    let status: String = order.try_get("status")?;
    let order_id_text = order_id.to_string();

    if status == "provisioned" {
        tx.rollback().await?;
        return Ok(Fulfilment::AlreadyProcessed);
    }
    if !matches!(status.as_str(), "created" | "pending" | "expired") {
        tx.rollback().await?;
        return Ok(Fulfilment::OrderNotPayable { status });
    }
    if tx_data.status != "success" {
        tx.rollback().await?;
        return Ok(Fulfilment::NotSuccessful { status: tx_data.status.clone() });
    }

    let amount: i64 = order.try_get("amount_kobo")?;
    let currency: String = order.try_get("currency")?;
    if amount != tx_data.amount_kobo || currency != tx_data.currency {
        // Money arrived but not the amount we asked for. Keep the evidence, hold access.
        insert_payment(&mut tx, tenant, order_id, tx_data, false).await?;
        sqlx::query("UPDATE orders SET status = 'refund_flagged' WHERE id = $1").bind(order_id).execute(&mut *tx).await?;
        audit(
            &mut tx,
            tenant,
            "paystack",
            None,
            "payment.amount_mismatch",
            Some(("order", &order_id_text)),
            serde_json::json!({
                "expected_kobo": amount, "received_kobo": tx_data.amount_kobo,
                "expected_currency": currency, "received_currency": tx_data.currency,
                "gateway_transaction": tx_data.id,
            }),
        )
        .await?;
        tx.commit().await?;
        return Ok(Fulfilment::AmountMismatch);
    }

    insert_payment(&mut tx, tenant, order_id, tx_data, true).await?;

    let account_id: Option<Uuid> = order.try_get("account_id")?;
    let purpose: String = order.try_get("purpose")?;

    // Wallet top-up: credit the member's wallet and stop. No plan, no code.
    if purpose == "topup" {
        let account = account_id.ok_or_else(|| AppError::Internal("top-up order has no account".into()))?;
        sqlx::query(
            "INSERT INTO wallet_ledger (tenant_id, account_id, kind, amount_kobo, order_id, note)
             VALUES ($1, $2, 'topup', $3, $4, 'Paystack top-up')",
        )
        .bind(tenant)
        .bind(account)
        .bind(amount)
        .bind(order_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE orders SET status = 'provisioned', paid_at = now() WHERE id = $1").bind(order_id).execute(&mut *tx).await?;
        audit(
            &mut tx,
            tenant,
            "paystack",
            None,
            "wallet.topup",
            Some(("order", &order_id_text)),
            serde_json::json!({ "gateway_transaction": tx_data.id, "amount_kobo": amount, "account_id": account }),
        )
        .await?;
        tx.commit().await?;
        return Ok(Fulfilment::WalletCredited { amount_kobo: amount });
    }

    // Who owns the entitlement: a member account, or a brand-new guest code.
    let phone: Option<String> = order.try_get("phone")?;
    let (access_code_id, access_code): (Option<Uuid>, Option<String>) = if account_id.is_some() {
        (None, None)
    } else {
        let (id, code) = create_access_code(&mut tx, tenant, phone.as_deref()).await?;
        (Some(id), Some(code))
    };

    sqlx::query("UPDATE orders SET status = 'paid', paid_at = now(), access_code_id = $2 WHERE id = $1")
        .bind(order_id)
        .bind(access_code_id)
        .execute(&mut *tx)
        .await?;

    // Grant through the engine so start/expiry rules live in exactly one place.
    let plan_id: Uuid = order.try_get::<Option<Uuid>, _>("plan_id")?.ok_or_else(|| AppError::Internal("plan order has no plan".into()))?;
    let plan_row = load_plan(&mut tx, plan_id).await?;
    let subject = match (account_id, access_code_id) {
        (Some(a), _) => Subject::Account(a),
        (None, Some(c)) => Subject::Code(c),
        (None, None) => return Err(AppError::Internal("order has no subject".into())),
    };
    let entitlement_id = grant(&mut tx, tenant, order_id, plan_id, &plan_row.plan, subject).await?;

    sqlx::query("UPDATE orders SET status = 'provisioned' WHERE id = $1").bind(order_id).execute(&mut *tx).await?;

    audit(
        &mut tx,
        tenant,
        "paystack",
        None,
        "payment.confirmed",
        Some(("order", &order_id_text)),
        serde_json::json!({
            "gateway_transaction": tx_data.id, "amount_kobo": tx_data.amount_kobo,
            "channel": tx_data.channel, "entitlement_id": entitlement_id,
            "subject": if account_id.is_some() { "account" } else { "guest_code" },
        }),
    )
    .await?;

    tx.commit().await?;
    Ok(Fulfilment::Provisioned { entitlement_id, access_code })
}

async fn insert_payment(conn: &mut sqlx::PgConnection, tenant: Uuid, order_id: Uuid, t: &VerifiedTransaction, verified: bool) -> Result<()> {
    sqlx::query(
        "INSERT INTO payments
            (tenant_id, order_id, provider, provider_event_id, event_type, amount_kobo, fees_kobo,
             currency, channel, verified, raw_event)
         VALUES ($1, $2, 'paystack', $3, 'charge.success', $4, $5, $6, $7, $8, $9)",
    )
    .bind(tenant)
    .bind(order_id)
    .bind(&t.id)
    .bind(t.amount_kobo)
    .bind(t.fees_kobo)
    .bind(&t.currency)
    .bind(&t.channel)
    .bind(verified)
    .bind(&t.raw)
    .execute(conn)
    .await?;
    Ok(())
}

/// Insert a fresh guest code, retrying on the (very unlikely) clash with an
/// existing one for this tenant.
async fn create_access_code(conn: &mut sqlx::PgConnection, tenant: Uuid, phone: Option<&str>) -> Result<(Uuid, String)> {
    for _ in 0..8 {
        let code = new_access_code();
        let row = sqlx::query(
            "INSERT INTO access_codes (tenant_id, code, phone) VALUES ($1, $2, $3)
             ON CONFLICT (tenant_id, code) DO NOTHING RETURNING id",
        )
        .bind(tenant)
        .bind(&code)
        .bind(phone)
        .fetch_optional(&mut *conn)
        .await?;
        if let Some(row) = row {
            return Ok((row.try_get("id")?, code));
        }
    }
    Err(AppError::Internal("could not allocate a unique access code".into()))
}
