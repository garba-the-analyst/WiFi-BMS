-- =====================================================================
-- 0003: site-agent credentials, connect tickets, member/admin sessions,
--       OTP codes, RADIUS session bookkeeping, wallet top-up orders
--
-- Run after 0002. Everything here is tenant-scoped with row-level security,
-- except node_token_lookup(), the one function that must find a tenant from
-- a credential alone (the site agent presents only its token).
-- =====================================================================

-- ---------------------------------------------------------------------
-- Orders can also be wallet top-ups (no plan, member only)
-- ---------------------------------------------------------------------
ALTER TABLE orders ADD COLUMN purpose text NOT NULL DEFAULT 'plan' CHECK (purpose IN ('plan', 'topup'));
ALTER TABLE orders ALTER COLUMN plan_id DROP NOT NULL;
ALTER TABLE orders ADD CONSTRAINT order_purpose_shape CHECK (
    (purpose = 'plan'  AND plan_id IS NOT NULL) OR
    (purpose = 'topup' AND plan_id IS NULL AND account_id IS NOT NULL)
);

-- ---------------------------------------------------------------------
-- Site-agent credentials
-- A node token is shown once when created; only its SHA-256 is stored.
-- ---------------------------------------------------------------------
CREATE TABLE node_tokens (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id    uuid        NOT NULL REFERENCES tenants(id),
    site_id      uuid        NOT NULL REFERENCES sites(id),
    name         text        NOT NULL,                       -- e.g. "Site 1 mini-PC"
    token_hash   text        NOT NULL UNIQUE CHECK (token_hash ~ '^[0-9a-f]{64}$'),
    created_at   timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz,
    revoked_at   timestamptz
);

-- Which tenant and site does this token belong to? Nothing else is revealed.
CREATE FUNCTION node_token_lookup(p_hash text)
RETURNS TABLE (token_id uuid, tenant_id uuid, site_id uuid)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = public, pg_temp AS $$
    SELECT id, tenant_id, site_id FROM node_tokens WHERE token_hash = p_hash AND revoked_at IS NULL
$$;
REVOKE ALL ON FUNCTION node_token_lookup(text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION node_token_lookup(text) TO platform_app;

-- ---------------------------------------------------------------------
-- Connect tickets: how a customer gets from the portal onto the hotspot.
-- The portal checks the access code or member sign-in, then asks for a ticket.
-- The ticket's username/password are what the hotspot sends to RADIUS. They
-- work once, for one MAC address, for two minutes.
-- ---------------------------------------------------------------------
CREATE TABLE connect_tickets (
    id                  uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id           uuid        NOT NULL REFERENCES tenants(id),
    site_id             uuid        NOT NULL REFERENCES sites(id),
    username            text        NOT NULL,
    password_hash       text        NOT NULL,
    mac                 macaddr     NOT NULL,
    account_id          uuid        REFERENCES accounts(id),
    access_code_id      uuid        REFERENCES access_codes(id),
    entitlement_id      uuid        REFERENCES entitlements(id),   -- filled when authorised
    device_id           uuid        REFERENCES devices(id),        -- filled when authorised
    created_at          timestamptz NOT NULL DEFAULT now(),
    expires_at          timestamptz NOT NULL,
    authorised_at       timestamptz,
    UNIQUE (tenant_id, username),
    CONSTRAINT ticket_has_one_subject CHECK ((account_id IS NOT NULL) <> (access_code_id IS NOT NULL))
);
CREATE INDEX connect_tickets_expiry_idx ON connect_tickets (expires_at);

-- ---------------------------------------------------------------------
-- Member and admin sessions (bearer tokens, stored hashed)
-- ---------------------------------------------------------------------
CREATE TABLE member_sessions (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id    uuid        NOT NULL REFERENCES tenants(id),
    account_id   uuid        NOT NULL REFERENCES accounts(id),
    token_hash   text        NOT NULL UNIQUE,
    created_at   timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz NOT NULL,
    last_used_at timestamptz,
    ip           inet,
    user_agent   text,
    revoked_at   timestamptz
);

ALTER TABLE admin_users ADD COLUMN failed_logins integer     NOT NULL DEFAULT 0;
ALTER TABLE admin_users ADD COLUMN locked_until  timestamptz;
ALTER TABLE admin_users ADD COLUMN totp_enabled  boolean     NOT NULL DEFAULT false;
ALTER TABLE admin_users ADD COLUMN last_login_at timestamptz;

CREATE TABLE admin_sessions (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id      uuid        NOT NULL REFERENCES tenants(id),
    admin_user_id  uuid        NOT NULL REFERENCES admin_users(id),
    token_hash     text        NOT NULL UNIQUE,
    created_at     timestamptz NOT NULL DEFAULT now(),
    expires_at     timestamptz NOT NULL,
    last_used_at   timestamptz,
    ip             inet,
    user_agent     text,
    revoked_at     timestamptz
);

-- One-time SMS codes for sign-in and password reset. Only the hash is stored.
CREATE TABLE otp_codes (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   uuid        NOT NULL REFERENCES tenants(id),
    phone       text        NOT NULL,
    purpose     text        NOT NULL CHECK (purpose IN ('login', 'reset')),
    code_hash   text        NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz NOT NULL,
    attempts    integer     NOT NULL DEFAULT 0,
    consumed_at timestamptz
);
CREATE INDEX otp_codes_phone_idx ON otp_codes (tenant_id, phone, created_at DESC);

-- ---------------------------------------------------------------------
-- RADIUS bookkeeping on sessions and devices
-- ---------------------------------------------------------------------
ALTER TABLE sessions ADD COLUMN acct_session_id text;
ALTER TABLE sessions ADD COLUMN username        text;
ALTER TABLE sessions ADD COLUMN radius_bytes_in  bigint  NOT NULL DEFAULT 0 CHECK (radius_bytes_in  >= 0);  -- router's running totals, to compute deltas
ALTER TABLE sessions ADD COLUMN radius_bytes_out bigint  NOT NULL DEFAULT 0 CHECK (radius_bytes_out >= 0);
ALTER TABLE sessions ADD COLUMN radius_seconds   integer NOT NULL DEFAULT 0 CHECK (radius_seconds   >= 0);
ALTER TABLE sessions ADD COLUMN idle_secs        integer NOT NULL DEFAULT 0 CHECK (idle_secs        >= 0);
CREATE UNIQUE INDEX sessions_acct_idx ON sessions (site_id, acct_session_id) WHERE acct_session_id IS NOT NULL;

ALTER TABLE devices ADD COLUMN dhcp_vendor_class text;
ALTER TABLE entitlements ADD COLUMN last_swap_at timestamptz;     -- device-swap cooldown starts here

-- ---------------------------------------------------------------------
-- Row-level security and grants for the new tables
-- ---------------------------------------------------------------------
DO $$
DECLARE t text;
BEGIN
    FOREACH t IN ARRAY ARRAY['node_tokens', 'connect_tickets', 'member_sessions', 'admin_sessions', 'otp_codes']
    LOOP
        EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', t);
        EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', t);
        EXECUTE format('CREATE POLICY tenant_isolation ON %I USING (tenant_id = current_tenant()) WITH CHECK (tenant_id = current_tenant())', t);
    END LOOP;
END $$;

GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO platform_app;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO platform_app;
REVOKE INSERT, UPDATE, DELETE ON tenants FROM platform_app;
