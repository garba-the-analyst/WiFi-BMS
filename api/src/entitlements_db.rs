//! Storing entitlements, and converting between database rows and the
//! entitlement engine's types.
//!
//! The engine is pure (no database, no clock). This module is the only place
//! that knows how its fields map to columns, so a schema change touches one file.

use std::time::{SystemTime, UNIX_EPOCH};

use entitlement::{Entitlement, Plan, Status};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use crate::error::{AppError, Result};

/// Seconds since 1970 according to this machine. Entitlement rules use this;
/// timestamps stored for humans use the database clock.
pub fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).expect("clock after 1970").as_secs() as i64
}

/// Who owns an entitlement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subject {
    Account(Uuid),
    Code(Uuid),
}

/// An entitlement as stored, plus the fields the engine does not model.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub id: Uuid,
    pub ent: Entitlement,
    pub last_swap_at: Option<i64>,
}

fn status_from(s: &str) -> Result<Status> {
    Ok(match s {
        "pending" => Status::Pending,
        "active" => Status::Active,
        "exhausted" => Status::Exhausted,
        "expired" => Status::Expired,
        "cancelled" => Status::Cancelled,
        other => return Err(AppError::Internal(format!("unknown entitlement status {other}"))),
    })
}

pub fn status_str(s: Status) -> &'static str {
    match s {
        Status::Pending => "pending",
        Status::Active => "active",
        Status::Exhausted => "exhausted",
        Status::Expired => "expired",
        Status::Cancelled => "cancelled",
    }
}

const SELECT: &str = "
    SELECT id, plan_snapshot, status::text AS status,
           extract(epoch FROM granted_at)::bigint   AS granted,
           extract(epoch FROM activated_at)::bigint AS activated,
           extract(epoch FROM expires_at)::bigint   AS expires,
           remaining_active_seconds, remaining_bytes,
           (fup_day - date '1970-01-01')            AS fup_day,
           fup_bytes_today,
           extract(epoch FROM last_swap_at)::bigint AS last_swap
      FROM entitlements";

fn from_row(row: &PgRow) -> Result<Loaded> {
    let id: Uuid = row.try_get("id")?;
    let plan: Plan =
        serde_json::from_value(row.try_get("plan_snapshot")?).map_err(|e| AppError::Internal(format!("stored plan snapshot is unreadable: {e}")))?;
    let ent = Entitlement {
        id: id.to_string(),
        plan,
        status: status_from(&row.try_get::<String, _>("status")?)?,
        granted_at: row.try_get("granted")?,
        activated_at: row.try_get("activated")?,
        expires_at: row.try_get::<Option<i64>, _>("expires")?.ok_or_else(|| AppError::Internal("entitlement has no expiry".into()))?,
        remaining_active_secs: row.try_get::<Option<i32>, _>("remaining_active_seconds")?.map(|v| v as u32),
        remaining_bytes: row.try_get::<Option<i64>, _>("remaining_bytes")?.map(|v| v as u64),
        fup_day: row.try_get::<Option<i32>, _>("fup_day")?.map(i64::from),
        fup_bytes_today: row.try_get::<i64, _>("fup_bytes_today")? as u64,
    };
    Ok(Loaded { id, ent, last_swap_at: row.try_get("last_swap")? })
}

/// The purchases a subject can still use (pending or active), locked for
/// update so two sessions cannot spend the same minutes at once.
pub async fn load_usable_for_update(conn: &mut PgConnection, subject: Subject) -> Result<Vec<Loaded>> {
    let (col, id) = match subject {
        Subject::Account(i) => ("account_id", i),
        Subject::Code(i) => ("access_code_id", i),
    };
    let rows = sqlx::query(&format!("{SELECT} WHERE {col} = $1 AND status IN ('pending', 'active') ORDER BY expires_at, granted_at FOR UPDATE"))
        .bind(id)
        .fetch_all(conn)
        .await?;
    rows.iter().map(from_row).collect()
}

pub async fn load_for_update(conn: &mut PgConnection, id: Uuid) -> Result<Loaded> {
    let row = sqlx::query(&format!("{SELECT} WHERE id = $1 FOR UPDATE")).bind(id).fetch_optional(conn).await?.ok_or(AppError::NotFound)?;
    from_row(&row)
}

/// Write the engine's view back to the row.
pub async fn save(conn: &mut PgConnection, l: &Loaded) -> Result<()> {
    sqlx::query(
        "UPDATE entitlements SET
            status = $2::entitlement_status,
            activated_at = CASE WHEN $3::bigint IS NULL THEN NULL ELSE to_timestamp($3::bigint) END,
            expires_at = to_timestamp($4::bigint),
            remaining_active_seconds = $5,
            remaining_bytes = $6,
            fup_day = CASE WHEN $7::int IS NULL THEN NULL ELSE date '1970-01-01' + $7::int END,
            fup_bytes_today = $8,
            last_swap_at = CASE WHEN $9::bigint IS NULL THEN NULL ELSE to_timestamp($9::bigint) END
          WHERE id = $1",
    )
    .bind(l.id)
    .bind(status_str(l.ent.status))
    .bind(l.ent.activated_at)
    .bind(l.ent.expires_at)
    .bind(l.ent.remaining_active_secs.map(|v| v as i32))
    .bind(l.ent.remaining_bytes.map(|v| v as i64))
    .bind(l.ent.fup_day.map(|v| v as i32))
    .bind(l.ent.fup_bytes_today as i64)
    .bind(l.last_swap_at)
    .execute(conn)
    .await?;
    Ok(())
}

/// Grant a new entitlement for a paid order, through the engine, and store it.
/// The plan is frozen into `plan_snapshot` so later edits never change it.
pub async fn grant(conn: &mut PgConnection, tenant: Uuid, order_id: Uuid, plan_id: Uuid, plan: &Plan, subject: Subject) -> Result<Uuid> {
    let id = Uuid::new_v4();
    let ent = Entitlement::grant(id.to_string(), plan.clone(), now_secs()).map_err(|e| AppError::Internal(format!("cannot grant plan: {e:?}")))?;
    let snapshot = serde_json::to_value(plan).map_err(|e| AppError::Internal(e.to_string()))?;
    let (account_id, code_id) = match subject {
        Subject::Account(a) => (Some(a), None),
        Subject::Code(c) => (None, Some(c)),
    };
    sqlx::query(
        "INSERT INTO entitlements
            (id, tenant_id, order_id, plan_id, plan_snapshot, account_id, access_code_id, status,
             activated_at, expires_at, remaining_active_seconds, remaining_bytes)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8::entitlement_status,
                 CASE WHEN $9::bigint IS NULL THEN NULL ELSE to_timestamp($9::bigint) END,
                 to_timestamp($10::bigint), $11, $12)",
    )
    .bind(id)
    .bind(tenant)
    .bind(order_id)
    .bind(plan_id)
    .bind(snapshot)
    .bind(account_id)
    .bind(code_id)
    .bind(status_str(ent.status))
    .bind(ent.activated_at)
    .bind(ent.expires_at)
    .bind(ent.remaining_active_secs.map(|v| v as i32))
    .bind(ent.remaining_bytes.map(|v| v as i64))
    .execute(conn)
    .await?;
    Ok(id)
}

/// Seconds the site's local time is ahead of UTC (Lagos: 3600). Fair-use days
/// reset at LOCAL midnight, so the meter needs this.
pub async fn site_tz_offset(conn: &mut PgConnection, site_id: Uuid) -> Result<i32> {
    let off: Option<i32> = sqlx::query_scalar(
        "SELECT extract(epoch FROM (now() AT TIME ZONE timezone) - (now() AT TIME ZONE 'UTC'))::int
           FROM sites WHERE id = $1",
    )
    .bind(site_id)
    .fetch_optional(conn)
    .await?;
    off.ok_or(AppError::NotFound)
}
