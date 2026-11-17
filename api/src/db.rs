//! Database helpers: tenant-scoped transactions, the audit trail, and loading
//! a plan into the entitlement engine's type.

use entitlement::{Burst, FairUse, Plan, PlanKind, SpeedProfile};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use crate::error::{AppError, Result};

/// Start a transaction that can see only `tenant`'s rows.
///
/// `set_config(..., true)` makes the setting local to this transaction, so a
/// pooled connection never carries one tenant's id into another request. The
/// row-level-security policies in the schema read it.
pub async fn tenant_tx(pool: &PgPool, tenant: Uuid) -> Result<Transaction<'static, Postgres>> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('app.tenant_id', $1, true)").bind(tenant.to_string()).execute(&mut *tx).await?;
    Ok(tx)
}

/// Find a tenant by its URL slug. Uses the one SECURITY DEFINER function that
/// is allowed to look across tenants.
pub async fn tenant_id_by_slug(pool: &PgPool, slug: &str) -> Result<Uuid> {
    sqlx::query_scalar::<_, Option<Uuid>>("SELECT tenant_id_by_slug($1)").bind(slug).fetch_one(pool).await?.ok_or(AppError::NotFound)
}

/// Append to the tamper-evident audit log. The database trigger computes the
/// hash chain; callers only say what happened.
pub async fn audit(
    conn: &mut PgConnection,
    tenant: Uuid,
    actor_kind: &str,
    actor_id: Option<&str>,
    action: &str,
    entity: Option<(&str, &str)>,
    detail: serde_json::Value,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO audit_log (tenant_id, actor_kind, actor_id, action, entity, entity_id, detail)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(tenant)
    .bind(actor_kind)
    .bind(actor_id)
    .bind(action)
    .bind(entity.map(|e| e.0))
    .bind(entity.map(|e| e.1))
    .bind(detail)
    .execute(conn)
    .await?;
    Ok(())
}

fn speed_from(row: &PgRow, prefix: &str) -> Result<Option<SpeedProfile>> {
    let col = |n: &str| format!("{prefix}{n}");
    let down: Option<i32> = row.try_get(col("down_kbps").as_str())?;
    let Some(down) = down else { return Ok(None) };
    let up: i32 = row.try_get(col("up_kbps").as_str())?;
    let b_down: Option<i32> = row.try_get(col("burst_down_kbps").as_str())?;
    let b_up: Option<i32> = row.try_get(col("burst_up_kbps").as_str())?;
    let b_thr: Option<i32> = row.try_get(col("burst_threshold_kbps").as_str())?;
    let b_time: Option<i32> = row.try_get(col("burst_time_s").as_str())?;
    let prio: i16 = row.try_get(col("priority").as_str())?;
    // A burst needs all four numbers; a partly filled one is ignored.
    let burst = match (b_down, b_up, b_thr, b_time) {
        (Some(d), Some(u), Some(t), Some(s)) => Some(Burst { down_kbps: d as u32, up_kbps: u as u32, threshold_kbps: t as u32, time_secs: s as u32 }),
        _ => None,
    };
    Ok(Some(SpeedProfile { down_kbps: down as u32, up_kbps: up as u32, burst, priority: prio as u8 }))
}

/// Everything the storefront needs to show and sell a plan.
pub struct PlanRow {
    pub id: Uuid,
    pub name: String,
    pub price_kobo: i64,
    pub plan: Plan,
}

const PLAN_SELECT: &str = "
    SELECT p.id, p.name, p.kind::text AS kind, p.price_kobo, p.active_seconds, p.data_bytes,
           p.valid_seconds, p.activate_on_first_use, p.unused_expiry_seconds, p.device_limit,
           p.swap_cooldown_s, p.active_threshold_bytes, p.idle_timeout_s, p.fup_daily_bytes,
           s.down_kbps, s.up_kbps, s.burst_down_kbps, s.burst_up_kbps,
           s.burst_threshold_kbps, s.burst_time_s, s.priority,
           f.down_kbps AS f_down_kbps, f.up_kbps AS f_up_kbps, f.burst_down_kbps AS f_burst_down_kbps,
           f.burst_up_kbps AS f_burst_up_kbps, f.burst_threshold_kbps AS f_burst_threshold_kbps,
           f.burst_time_s AS f_burst_time_s, f.priority AS f_priority
      FROM plans p
      JOIN speed_profiles s ON s.id = p.speed_profile_id
      LEFT JOIN speed_profiles f ON f.id = p.fup_speed_profile_id";

fn plan_from(row: &PgRow) -> Result<PlanRow> {
    let kind = match row.try_get::<String, _>("kind")?.as_str() {
        "time" => PlanKind::Time,
        "data" => PlanKind::Data,
        "hybrid" => PlanKind::Hybrid,
        other => return Err(AppError::Internal(format!("unknown plan kind {other}"))),
    };
    let speed = speed_from(row, "")?.ok_or_else(|| AppError::Internal("plan has no speed profile".into()))?;
    let fair_use = match row.try_get::<Option<i64>, _>("fup_daily_bytes")? {
        Some(daily) => {
            let throttled = speed_from(row, "f_")?.ok_or_else(|| AppError::Internal("fair-use plan has no slowed profile".into()))?;
            Some(FairUse { daily_bytes: daily as u64, throttled })
        }
        None => None,
    };
    let plan = Plan {
        kind,
        active_seconds: row.try_get::<Option<i32>, _>("active_seconds")?.map(|v| v as u32),
        data_bytes: row.try_get::<Option<i64>, _>("data_bytes")?.map(|v| v as u64),
        valid_seconds: row.try_get::<i32, _>("valid_seconds")? as u32,
        activate_on_first_use: row.try_get("activate_on_first_use")?,
        unused_expiry_seconds: row.try_get::<i32, _>("unused_expiry_seconds")? as u32,
        device_limit: row.try_get::<i16, _>("device_limit")? as u8,
        swap_cooldown_secs: row.try_get::<i32, _>("swap_cooldown_s")? as u32,
        speed,
        active_threshold_bytes: row.try_get::<i32, _>("active_threshold_bytes")? as u64,
        idle_timeout_secs: row.try_get::<i32, _>("idle_timeout_s")? as u32,
        fair_use,
    };
    plan.validate().map_err(|e| AppError::Internal(format!("stored plan is invalid: {e:?}")))?;
    Ok(PlanRow { id: row.try_get("id")?, name: row.try_get("name")?, price_kobo: row.try_get("price_kobo")?, plan })
}

/// Load one plan (any state). Used when fulfilling a paid order.
pub async fn load_plan(conn: &mut PgConnection, plan_id: Uuid) -> Result<PlanRow> {
    let row = sqlx::query(&format!("{PLAN_SELECT} WHERE p.id = $1")).bind(plan_id).fetch_optional(conn).await?.ok_or(AppError::NotFound)?;
    plan_from(&row)
}

/// Plans a customer may buy right now, cheapest first.
pub async fn list_active_plans(conn: &mut PgConnection) -> Result<Vec<PlanRow>> {
    let rows = sqlx::query(&format!("{PLAN_SELECT} WHERE p.is_active ORDER BY p.price_kobo, p.name")).fetch_all(conn).await?;
    rows.iter().map(plan_from).collect()
}
