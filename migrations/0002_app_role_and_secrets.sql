-- =====================================================================
-- 0002: application role, per-tenant payment secrets, tenant lookup
--
-- Run after 0001_init.sql, as the database owner.
--
-- * platform_app   the role the API connects as. It is NOT a superuser and
--                  does NOT own the tables, so row-level security always
--                  applies to it. Create your login role as a member:
--                      CREATE ROLE api_login LOGIN PASSWORD '...' IN ROLE platform_app;
-- * tenant_secrets holds each tenant's Paystack secret key, encrypted by the
--                  API (AES-256-GCM) before it is written. The database never
--                  sees the plain key or the master key.
-- * tenant_id_by_slug()
--                  Webhooks arrive before we know the tenant. RLS hides every
--                  tenant from a session with no tenant set, so this one
--                  narrow SECURITY DEFINER function answers "which tenant id
--                  belongs to this slug" and nothing else.
-- =====================================================================

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'platform_app') THEN
        CREATE ROLE platform_app NOLOGIN;
    END IF;
END $$;

CREATE TABLE tenant_secrets (
    tenant_id           uuid PRIMARY KEY REFERENCES tenants(id),
    paystack_secret_enc text        NOT NULL,   -- base64(nonce || ciphertext), see api/src/secrets.rs
    updated_at          timestamptz NOT NULL DEFAULT now()
);

ALTER TABLE tenant_secrets ENABLE ROW LEVEL SECURITY;
ALTER TABLE tenant_secrets FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON tenant_secrets
    USING (tenant_id = current_tenant()) WITH CHECK (tenant_id = current_tenant());

CREATE FUNCTION tenant_id_by_slug(p_slug text) RETURNS uuid
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = public, pg_temp AS $$
    SELECT id FROM tenants WHERE slug = p_slug
$$;
REVOKE ALL ON FUNCTION tenant_id_by_slug(text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION tenant_id_by_slug(text) TO platform_app;

-- The app may read and write tenant data. Append-only tables stay protected by
-- their forbid_mutation triggers even though UPDATE/DELETE are granted here.
GRANT USAGE ON SCHEMA public TO platform_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO platform_app;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO platform_app;
GRANT EXECUTE ON FUNCTION current_tenant() TO platform_app;
GRANT EXECUTE ON FUNCTION verify_audit_chain(uuid) TO platform_app;
-- tenants are created and renamed by the operator tooling, not by request handlers.
REVOKE INSERT, UPDATE, DELETE ON tenants FROM platform_app;

-- A paid order whose amount did not match is held as 'refund_flagged'. For a
-- guest there is no account or code yet, so that status must be allowed on a
-- subject-less order alongside the pre-payment states.
ALTER TABLE orders DROP CONSTRAINT order_has_one_subject;
ALTER TABLE orders ADD CONSTRAINT order_has_one_subject CHECK (
    (account_id IS NOT NULL AND access_code_id IS NULL) OR
    (account_id IS NULL AND access_code_id IS NOT NULL) OR
    (account_id IS NULL AND access_code_id IS NULL
        AND status IN ('created', 'pending', 'failed', 'expired', 'refund_flagged')
        AND phone IS NOT NULL)
);
