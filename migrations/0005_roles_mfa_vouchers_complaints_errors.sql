-- 0005: staff roles, 2-step verification methods, manual and special vouchers,
-- complaints, the error log, and a default 2 Mbps speed profile.
-- Run after 0004, as the database owner (deploy/migrate.sh does this).

-- ---------------------------------------------------------------------
-- Staff roles: system_admin, ceo, admin, customer_rep
--   (the old owner / operator / viewer become system_admin / admin / customer_rep)
-- ---------------------------------------------------------------------
ALTER TABLE admin_users ALTER COLUMN role DROP DEFAULT;
ALTER TABLE admin_users ALTER COLUMN role TYPE text USING (
    CASE role::text WHEN 'owner' THEN 'system_admin' WHEN 'operator' THEN 'admin' ELSE 'customer_rep' END);
ALTER TABLE admin_users ALTER COLUMN role SET DEFAULT 'customer_rep';
ALTER TABLE admin_users ADD CONSTRAINT admin_users_role_check CHECK (role IN ('system_admin', 'ceo', 'admin', 'customer_rep'));
DROP TYPE admin_role;

ALTER TABLE admin_users ADD COLUMN display_name text;
ALTER TABLE admin_users ADD COLUMN phone        text;                 -- for SMS codes
ALTER TABLE admin_users ADD COLUMN created_by   uuid REFERENCES admin_users(id);
-- 2-step verification method. 'none' = not enrolled yet: the account can sign in but may only enrol.
ALTER TABLE admin_users ADD COLUMN mfa_method   text NOT NULL DEFAULT 'none' CHECK (mfa_method IN ('none', 'totp', 'email', 'sms'));
UPDATE admin_users SET mfa_method = 'totp' WHERE totp_enabled;

-- One-time codes for sign-in (purpose 'login') and for enrolling email/SMS (purpose 'enrol').
CREATE TABLE admin_challenges (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   uuid        NOT NULL REFERENCES tenants(id),
    admin_id    uuid        NOT NULL REFERENCES admin_users(id),
    purpose     text        NOT NULL CHECK (purpose IN ('login', 'enrol')),
    method      text        NOT NULL CHECK (method IN ('totp', 'email', 'sms')),
    code_hash   text,                                  -- sha256 of the code; null for totp
    target      text,                                  -- phone number being enrolled (sms)
    attempts    integer     NOT NULL DEFAULT 0,
    expires_at  timestamptz NOT NULL,
    used_at     timestamptz,
    ip          inet,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX admin_challenges_admin_idx ON admin_challenges (admin_id, created_at DESC);

-- ---------------------------------------------------------------------
-- Vouchers: paid (online), manual (staff took cash), special (unlimited, staff only)
-- ---------------------------------------------------------------------
ALTER TABLE access_codes ADD COLUMN kind      text NOT NULL DEFAULT 'paid' CHECK (kind IN ('paid', 'manual', 'special'));
ALTER TABLE access_codes ADD COLUMN issued_by uuid REFERENCES admin_users(id);
ALTER TABLE access_codes ADD COLUMN label     text;                   -- who it is for / why
ALTER TABLE access_codes ADD COLUMN revoked_at timestamptz;

-- Special vouchers use a hidden plan made for them; it is never sold or listed.
ALTER TABLE plans ADD COLUMN is_special  boolean NOT NULL DEFAULT false;
ALTER TABLE plans ADD COLUMN description text;
ALTER TABLE orders ADD COLUMN issued_by uuid REFERENCES admin_users(id);   -- staff member who took a manual payment

-- ---------------------------------------------------------------------
-- Complaints
-- ---------------------------------------------------------------------
CREATE TABLE complaints (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id    uuid        NOT NULL REFERENCES tenants(id),
    account_id   uuid        REFERENCES accounts(id),
    phone        text        NOT NULL,
    order_ref    text,
    category     text        NOT NULL DEFAULT 'other' CHECK (category IN ('payment', 'connection', 'speed', 'voucher', 'account', 'other')),
    message      text        NOT NULL CHECK (char_length(message) BETWEEN 3 AND 2000),
    status       text        NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'in_progress', 'resolved')),
    assigned_to  uuid        REFERENCES admin_users(id),
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    resolved_at  timestamptz
);
CREATE INDEX complaints_tenant_status_idx ON complaints (tenant_id, status, created_at DESC);
CREATE TABLE complaint_notes (
    id           bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id    uuid        NOT NULL REFERENCES tenants(id),
    complaint_id uuid        NOT NULL REFERENCES complaints(id),
    admin_id     uuid        REFERENCES admin_users(id),
    body         text        NOT NULL CHECK (char_length(body) BETWEEN 1 AND 2000),
    created_at   timestamptz NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------
-- Error log: system, web app and hardware errors, with enough detail to fix them
-- ---------------------------------------------------------------------
CREATE TABLE error_events (
    id           bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id    uuid        NOT NULL REFERENCES tenants(id),
    ts           timestamptz NOT NULL DEFAULT now(),
    source       text        NOT NULL CHECK (source IN ('api', 'database', 'payment', 'sms', 'email', 'webapp', 'hardware', 'radius', 'site_agent', 'security', 'system')),
    severity     text        NOT NULL CHECK (severity IN ('info', 'warning', 'error', 'critical')),
    code         text        NOT NULL,                 -- short stable name, e.g. payment.gateway
    message      text        NOT NULL,
    details      jsonb       NOT NULL DEFAULT '{}'::jsonb,   -- route, status, request id, stack, device, node ...
    hint         text,                                 -- what to check first
    fingerprint  text        NOT NULL,                 -- same problem = same fingerprint (used to group)
    request_id   text,
    site_id      uuid        REFERENCES sites(id),
    node_id      uuid,
    admin_id     uuid,
    resolved_at  timestamptz,
    resolved_by  uuid
);
CREATE INDEX error_events_tenant_ts_idx ON error_events (tenant_id, ts DESC);
CREATE INDEX error_events_group_idx ON error_events (tenant_id, fingerprint, ts DESC);

-- ---------------------------------------------------------------------
-- Row-level security and grants for the new tables
-- ---------------------------------------------------------------------
DO $$
DECLARE t text;
BEGIN
  FOREACH t IN ARRAY ARRAY['admin_challenges', 'complaints', 'complaint_notes', 'error_events']
  LOOP
    EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', t);
    EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', t);
    EXECUTE format('CREATE POLICY tenant_isolation ON %I USING (tenant_id = current_tenant()) WITH CHECK (tenant_id = current_tenant())', t);
  END LOOP;
END $$;
GRANT SELECT, INSERT, UPDATE, DELETE ON admin_challenges, complaints, complaint_notes, error_events TO platform_app;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO platform_app;

-- ---------------------------------------------------------------------
-- Every tenant gets a default 2 Mbps (up and down) profile.
-- ---------------------------------------------------------------------
INSERT INTO speed_profiles (tenant_id, name, down_kbps, up_kbps)
SELECT t.id, 'Standard 2 Mbps', 2000, 2000 FROM tenants t
 WHERE NOT EXISTS (SELECT 1 FROM speed_profiles s WHERE s.tenant_id = t.id AND s.down_kbps = 2000 AND s.up_kbps = 2000);

-- Background jobs (offline-node detection, log pruning) need to visit every tenant.
-- Like tenant_id_by_slug, this narrow function reveals ids only.
CREATE FUNCTION all_tenant_ids() RETURNS SETOF uuid
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = public, pg_temp AS $$
    SELECT id FROM tenants
$$;
REVOKE ALL ON FUNCTION all_tenant_ids() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION all_tenant_ids() TO platform_app;

-- Creating an establishment (the `create-tenant` command). Only inserts the tenant row; everything else
-- the command creates afterwards goes through ordinary row-level security.
CREATE FUNCTION provision_tenant(p_id uuid, p_slug text, p_name text, p_theme jsonb) RETURNS void
LANGUAGE sql SECURITY DEFINER SET search_path = public, pg_temp AS $$
    INSERT INTO tenants (id, slug, name, theme) VALUES (p_id, p_slug, p_name, p_theme)
$$;
REVOKE ALL ON FUNCTION provision_tenant(uuid, text, text, jsonb) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION provision_tenant(uuid, text, text, jsonb) TO platform_app;

-- The app role may only READ the tenants table. Branding edits go through this function, which refuses
-- to touch any tenant other than the one the session is working for.
CREATE FUNCTION update_tenant_theme(p_patch jsonb) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER SET search_path = public, pg_temp AS $$
BEGIN
    IF current_tenant() IS NULL THEN
        RAISE EXCEPTION 'no tenant in session';
    END IF;
    UPDATE tenants SET theme = theme || p_patch WHERE id = current_tenant();
END
$$;
REVOKE ALL ON FUNCTION update_tenant_theme(jsonb) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION update_tenant_theme(jsonb) TO platform_app;
