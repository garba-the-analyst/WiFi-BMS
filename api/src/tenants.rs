//! One installation, many establishments: creating a tenant, and the branding the web app shows.
//!
//! A new client (a cafe, a hotel, an estate) is one `create-tenant` command and one
//! `create-admin` command. Their data is separated from every other tenant by row-level security,
//! their staff, plans, customers, Paystack key and error log are their own, and the web app takes
//! its name, colour and support number from [`brand`], so no code changes are needed.

use axum::http::HeaderMap;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::admin::{authenticate, Perm, DEFAULT_KBPS};
use crate::db::{audit, tenant_id_by_slug, tenant_tx};
use crate::error::{AppError, Result};
use crate::AppState;

fn valid_slug(s: &str) -> bool {
    (2..=40).contains(&s.len()) && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') && !s.starts_with('-') && !s.ends_with('-')
}

/// Create an establishment with its first site and the default 2 Mbps speed profile.
/// Run by the `create-tenant` command. Returns the tenant id.
pub async fn create_tenant(pool: &PgPool, slug: &str, name: &str, site_slug: &str, site_name: &str) -> Result<Uuid> {
    if !valid_slug(slug) || !valid_slug(site_slug) {
        return Err(AppError::BadRequest("slugs use lower-case letters, digits and dashes (2 to 40 characters)".into()));
    }
    if name.trim().is_empty() || site_name.trim().is_empty() {
        return Err(AppError::BadRequest("a name is required".into()));
    }
    let id = Uuid::new_v4();
    // The tenants table only lets a session see its own row, so the id is chosen first and set as the session's tenant.
    let mut tx = tenant_tx(pool, id).await?;
    sqlx::query("SELECT provision_tenant($1, $2, $3, $4)")
        .bind(id)
        .bind(slug)
        .bind(name.trim())
        .bind(json!({ "brand": name.trim() }))
        .execute(&mut *tx)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(d) if d.is_unique_violation() => AppError::BadRequest("a tenant with that slug already exists".into()),
            _ => e.into(),
        })?;
    sqlx::query("INSERT INTO sites (tenant_id, name, slug) VALUES ($1, $2, $3)").bind(id).bind(site_name.trim()).bind(site_slug).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO speed_profiles (tenant_id, name, down_kbps, up_kbps) VALUES ($1, 'Standard 2 Mbps', $2, $2)")
        .bind(id)
        .bind(DEFAULT_KBPS)
        .execute(&mut *tx)
        .await?;
    audit(&mut tx, id, "system", None, "tenant.created", Some(("tenant", &id.to_string())), json!({ "slug": slug })).await?;
    tx.commit().await?;
    Ok(id)
}

/// Public: what the web app needs to look like this establishment.
pub async fn brand(st: &AppState, tenant_slug: &str) -> Result<Value> {
    let tenant = tenant_id_by_slug(&st.pool, tenant_slug).await?;
    let mut tx = tenant_tx(&st.pool, tenant).await?;
    let (name, theme): (String, Value) = sqlx::query_as("SELECT name, theme FROM tenants WHERE id = $1").bind(tenant).fetch_one(&mut *tx).await?;
    let sites: Value = sqlx::query_scalar(
        "SELECT COALESCE(jsonb_agg(jsonb_build_object('slug', slug, 'name', name) ORDER BY name), '[]'::jsonb) FROM sites WHERE status = 'active'",
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    let pick = |k: &str| theme.get(k).cloned().unwrap_or(Value::Null);
    Ok(json!({
        "name": name, "brand": if pick("brand").is_null() { json!(name) } else { pick("brand") },
        "tagline": pick("tagline"), "supportPhone": pick("supportPhone"), "primary": pick("primary"),
        "hotspotHosts": theme.get("hotspotHosts").cloned().unwrap_or(json!([])), "sites": sites,
    }))
}

#[derive(Debug, Deserialize)]
pub struct BrandIn {
    #[serde(default)]
    pub brand: Option<String>,
    #[serde(default)]
    pub tagline: Option<String>,
    #[serde(default, rename = "supportPhone")]
    pub support_phone: Option<String>,
    /// A colour like `#0F766E`.
    #[serde(default)]
    pub primary: Option<String>,
    /// Router addresses the portal may send a customer's login to, beyond private addresses and `.lan` names.
    #[serde(default, rename = "hotspotHosts")]
    pub hotspot_hosts: Option<Vec<String>>,
}

fn host_ok(h: &str) -> bool {
    (1..=100).contains(&h.len()) && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

pub async fn set_brand(st: &AppState, tenant: &str, h: &HeaderMap, req: BrandIn) -> Result<Value> {
    let a = authenticate(st, tenant, h, Perm::Brand).await?;
    let mut patch = serde_json::Map::new();
    if let Some(b) = req.brand.as_deref() {
        if b.trim().is_empty() || b.len() > 60 {
            return Err(AppError::BadRequest("the name must be 1 to 60 characters".into()));
        }
        patch.insert("brand".into(), json!(b.trim()));
    }
    if let Some(t) = req.tagline.as_deref() {
        if t.len() > 120 {
            return Err(AppError::BadRequest("the tagline is limited to 120 characters".into()));
        }
        patch.insert("tagline".into(), json!(t.trim()));
    }
    if let Some(p) = req.support_phone.as_deref() {
        if p.len() > 20 || !p.chars().all(|c| c.is_ascii_digit() || " +-()".contains(c)) {
            return Err(AppError::BadRequest("enter a phone number".into()));
        }
        patch.insert("supportPhone".into(), json!(p.trim()));
    }
    if let Some(c) = req.primary.as_deref() {
        if !(c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|x| x.is_ascii_hexdigit())) {
            return Err(AppError::BadRequest("the colour must look like #0F766E".into()));
        }
        patch.insert("primary".into(), json!(c));
    }
    if let Some(hs) = req.hotspot_hosts.as_ref() {
        if hs.len() > 10 || !hs.iter().all(|x| host_ok(x)) {
            return Err(AppError::BadRequest("up to 10 router addresses, using letters, digits, dots and dashes".into()));
        }
        patch.insert("hotspotHosts".into(), json!(hs));
    }
    let mut tx = tenant_tx(&st.pool, a.tenant).await?;
    sqlx::query("SELECT update_tenant_theme($1::jsonb)").bind(Value::Object(patch.clone())).execute(&mut *tx).await?;
    audit(&mut tx, a.tenant, "admin", Some(&a.actor()), "brand.updated", None, Value::Object(patch)).await?;
    tx.commit().await?;
    Ok(json!({ "ok": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_url_safe() {
        assert!(valid_slug("waste2light") && valid_slug("site-1"));
        assert!(!valid_slug("A") && !valid_slug("-x") && !valid_slug("has space") && !valid_slug("UPPER"));
    }

    #[test]
    fn router_hosts_are_plain() {
        assert!(host_ok("hotspot.example.com") && host_ok("10.5.50.1"));
        assert!(!host_ok("a/b") && !host_ok("") && !host_ok("x y"));
    }
}
