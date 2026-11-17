-- =====================================================================
-- Starlink Wi-Fi Platform: core schema, migration 0001
-- Target: PostgreSQL 14 or newer (tested on 16). TimescaleDB is optional.
--
-- Design rules
--   1. Multi-tenant. Every business table carries tenant_id, and row-level
--      security (RLS) isolates tenants. The application connects as a
--      NON-owner role and runs:  SELECT set_config('app.tenant_id', '<uuid>', true);
--      at the start of every transaction.
--   2. Money is stored in kobo (1 naira = 100 kobo) as bigint. Never floats.
--   3. Ledgers (usage, wallet, audit) are append-only. UPDATE and DELETE are
--      blocked by trigger. Balances are derived from the ledgers.
--   4. The audit log is hash-chained per tenant, so edits are detectable.
--   5. Everything that happened is a row with a timestamp (timestamptz, UTC).
-- =====================================================================

BEGIN;

-- ---------------------------------------------------------------------
-- Enumerations
-- ---------------------------------------------------------------------
CREATE TYPE plan_kind          AS ENUM ('time', 'data', 'hybrid');
CREATE TYPE order_status       AS ENUM ('created', 'pending', 'paid', 'provisioned', 'failed', 'expired', 'refund_flagged', 'refunded');
CREATE TYPE entitlement_status AS ENUM ('pending', 'active', 'exhausted', 'expired', 'cancelled');
CREATE TYPE node_kind          AS ENUM ('dish', 'router', 'switch', 'ap', 'radio', 'site_node', 'controller');
CREATE TYPE account_status     AS ENUM ('active', 'suspended', 'closed');
CREATE TYPE admin_role         AS ENUM ('owner', 'operator', 'viewer');
CREATE TYPE session_end_reason AS ENUM ('user', 'idle', 'time_exhausted', 'data_exhausted', 'expired', 'admin', 'tethering', 'node_lost', 'error');
CREATE TYPE wallet_entry_kind  AS ENUM ('topup', 'purchase', 'refund', 'adjustment');

-- ---------------------------------------------------------------------
-- Tenants, sites and network nodes
-- ---------------------------------------------------------------------
CREATE TABLE tenants (
    id                  uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name                text        NOT NULL,
    slug                text        NOT NULL UNIQUE CHECK (slug ~ '^[a-z0-9-]{2,40}$'),
    -- how this tenant gets paid: its own Paystack keys, or a Paystack subaccount
    payment_mode        text        NOT NULL DEFAULT 'own_keys' CHECK (payment_mode IN ('own_keys', 'subaccount')),
    paystack_subaccount text,
    theme               jsonb       NOT NULL DEFAULT '{}'::jsonb,   -- logo, colours, language
    created_at          timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE sites (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   uuid        NOT NULL REFERENCES tenants(id),
    name        text        NOT NULL,
    slug        text        NOT NULL CHECK (slug ~ '^[a-z0-9-]{2,40}$'),
    timezone    text        NOT NULL DEFAULT 'Africa/Lagos',
    status      text        NOT NULL DEFAULT 'active' CHECK (status IN ('planned', 'active', 'maintenance', 'retired')),
    latitude    double precision,
    longitude   double precision,
    created_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, slug)
);

CREATE TABLE nodes (
    id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id       uuid        NOT NULL REFERENCES tenants(id),
    site_id         uuid        NOT NULL REFERENCES sites(id),
    parent_node_id  uuid        REFERENCES nodes(id),      -- e.g. an AP hangs off a radio
    kind            node_kind   NOT NULL,
    name            text        NOT NULL,
    model           text,
    mac             macaddr,
    mgmt_ip         inet,
    firmware        text,
    distance_m      integer CHECK (distance_m IS NULL OR distance_m >= 0),   -- from the hub, for remote spots
    last_seen_at    timestamptz,
    created_at      timestamptz NOT NULL DEFAULT now(),
    UNIQUE (site_id, name)
);
CREATE INDEX nodes_site_idx ON nodes (site_id, kind);

-- ---------------------------------------------------------------------
-- Speed profiles and plans
-- ---------------------------------------------------------------------
CREATE TABLE speed_profiles (
    id                   uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id            uuid     NOT NULL REFERENCES tenants(id),
    name                 text     NOT NULL,
    down_kbps            integer  NOT NULL CHECK (down_kbps > 0),
    up_kbps              integer  NOT NULL CHECK (up_kbps > 0),
    burst_down_kbps      integer  CHECK (burst_down_kbps IS NULL OR burst_down_kbps >= down_kbps),
    burst_up_kbps        integer  CHECK (burst_up_kbps   IS NULL OR burst_up_kbps   >= up_kbps),
    burst_threshold_kbps integer,
    burst_time_s         integer,
    priority             smallint NOT NULL DEFAULT 8 CHECK (priority BETWEEN 1 AND 8),
    created_at           timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, name)
);

CREATE TABLE plans (
    id                     uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id              uuid       NOT NULL REFERENCES tenants(id),
    name                   text       NOT NULL,
    kind                   plan_kind  NOT NULL,
    price_kobo             bigint     NOT NULL CHECK (price_kobo >= 0),
    -- time budget counts ACTIVE seconds only (see entitlement engine)
    active_seconds         integer    CHECK (active_seconds > 0),
    data_bytes             bigint     CHECK (data_bytes > 0),
    valid_seconds          integer    NOT NULL CHECK (valid_seconds > 0),   -- validity window
    activate_on_first_use  boolean    NOT NULL DEFAULT true,
    unused_expiry_seconds  integer    NOT NULL DEFAULT 7776000 CHECK (unused_expiry_seconds > 0),  -- unactivated purchase lapses after 90 days
    device_limit           smallint   NOT NULL DEFAULT 1 CHECK (device_limit BETWEEN 1 AND 20),
    swap_cooldown_s        integer    NOT NULL DEFAULT 86400,               -- one device swap per 24 h by default
    speed_profile_id       uuid       NOT NULL REFERENCES speed_profiles(id),
    active_threshold_bytes integer    NOT NULL DEFAULT 20480,               -- 20 KB per accounting interval = active
    idle_timeout_s         integer    NOT NULL DEFAULT 300,
    fup_daily_bytes        bigint     CHECK (fup_daily_bytes IS NULL OR fup_daily_bytes > 0),
    fup_speed_profile_id   uuid       REFERENCES speed_profiles(id),
    is_active              boolean    NOT NULL DEFAULT true,
    created_at             timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, name),
    -- each plan kind must carry the budgets it needs, and only those
    CONSTRAINT plan_budget_matches_kind CHECK (
        (kind = 'time'   AND active_seconds IS NOT NULL AND data_bytes IS NULL) OR
        (kind = 'data'   AND data_bytes     IS NOT NULL AND active_seconds IS NULL) OR
        (kind = 'hybrid' AND data_bytes     IS NOT NULL AND active_seconds IS NOT NULL)
    ),
    CONSTRAINT fup_needs_profile CHECK ((fup_daily_bytes IS NULL) = (fup_speed_profile_id IS NULL))
);

-- ---------------------------------------------------------------------
-- Identity: member accounts, guest access codes, consent
-- ---------------------------------------------------------------------
CREATE TABLE accounts (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id      uuid           NOT NULL REFERENCES tenants(id),
    phone          text           NOT NULL CHECK (phone ~ '^\+?[0-9]{8,15}$'),
    display_name   text,
    email          text,
    password_hash  text,                                  -- Argon2id; null when the member uses SMS codes only
    status         account_status NOT NULL DEFAULT 'active',
    failed_logins  integer        NOT NULL DEFAULT 0,
    locked_until   timestamptz,
    created_at     timestamptz    NOT NULL DEFAULT now(),
    last_login_at  timestamptz,
    UNIQUE (tenant_id, phone)
);

CREATE TABLE access_codes (
    id                    uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id             uuid        NOT NULL REFERENCES tenants(id),
    code                  text        NOT NULL CHECK (code ~ '^[A-HJ-NP-Z2-9]{8}$'),   -- no 0, 1, I, O
    phone                 text,
    claimed_by_account_id uuid        REFERENCES accounts(id),
    created_at            timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, code)
);

CREATE TABLE consents (
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id     uuid        NOT NULL REFERENCES tenants(id),
    account_id    uuid        REFERENCES accounts(id),
    access_code_id uuid       REFERENCES access_codes(id),
    phone         text,
    terms_version text        NOT NULL,
    ip            inet,
    user_agent    text,
    accepted_at   timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE admin_users (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id      uuid        NOT NULL REFERENCES tenants(id),
    email          text        NOT NULL,
    password_hash  text        NOT NULL,
    role           admin_role  NOT NULL DEFAULT 'viewer',
    totp_secret    bytea,                                   -- encrypted by the application
    status         account_status NOT NULL DEFAULT 'active',
    created_at     timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, email)
);

-- ---------------------------------------------------------------------
-- Orders and payments (Paystack)
-- ---------------------------------------------------------------------
CREATE TABLE orders (
    id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id       uuid         NOT NULL REFERENCES tenants(id),
    site_id         uuid         NOT NULL REFERENCES sites(id),
    plan_id         uuid         NOT NULL REFERENCES plans(id),
    reference       text         NOT NULL UNIQUE,          -- sent to Paystack; makes webhooks idempotent
    amount_kobo     bigint       NOT NULL CHECK (amount_kobo >= 0),
    currency        char(3)      NOT NULL DEFAULT 'NGN',
    status          order_status NOT NULL DEFAULT 'created',
    account_id      uuid         REFERENCES accounts(id),
    access_code_id  uuid         REFERENCES access_codes(id),
    phone           text,
    paid_with       text         NOT NULL DEFAULT 'paystack' CHECK (paid_with IN ('paystack', 'wallet', 'voucher', 'cash')),
    created_at      timestamptz  NOT NULL DEFAULT now(),
    paid_at         timestamptz,
    -- the buyer is a member or a guest, never both, and a guest order needs a phone
    CONSTRAINT order_has_one_subject CHECK (
        (account_id IS NOT NULL AND access_code_id IS NULL) OR
        (account_id IS NULL AND access_code_id IS NOT NULL) OR
        (account_id IS NULL AND access_code_id IS NULL AND status IN ('created', 'pending', 'failed', 'expired') AND phone IS NOT NULL)
    )
);
CREATE INDEX orders_tenant_status_idx ON orders (tenant_id, status, created_at DESC);

CREATE TABLE payments (
    id                 uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id          uuid        NOT NULL REFERENCES tenants(id),
    order_id           uuid        NOT NULL REFERENCES orders(id),
    provider           text        NOT NULL DEFAULT 'paystack',
    provider_event_id  text        NOT NULL,              -- Paystack transaction id: one row per event
    event_type         text        NOT NULL,              -- e.g. charge.success
    amount_kobo        bigint      NOT NULL CHECK (amount_kobo >= 0),
    fees_kobo          bigint      NOT NULL DEFAULT 0 CHECK (fees_kobo >= 0),
    currency           char(3)     NOT NULL DEFAULT 'NGN',
    channel            text,                              -- card, bank_transfer, ussd ...
    verified           boolean     NOT NULL DEFAULT false,-- true after the Verify Transaction call matched
    raw_event          jsonb       NOT NULL,
    received_at        timestamptz NOT NULL DEFAULT now(),
    UNIQUE (provider, provider_event_id, event_type)       -- a repeated webhook cannot create a second row
);

-- ---------------------------------------------------------------------
-- Entitlements (what a customer owns) and the usage ledger
-- ---------------------------------------------------------------------
CREATE TABLE entitlements (
    id                       uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id                uuid               NOT NULL REFERENCES tenants(id),
    order_id                 uuid               REFERENCES orders(id),
    plan_id                  uuid               NOT NULL REFERENCES plans(id),
    plan_snapshot            jsonb              NOT NULL,   -- plan as sold, so later plan edits never change it
    account_id               uuid               REFERENCES accounts(id),
    access_code_id           uuid               REFERENCES access_codes(id),
    status                   entitlement_status NOT NULL DEFAULT 'pending',
    granted_at               timestamptz        NOT NULL DEFAULT now(),
    activated_at             timestamptz,
    expires_at               timestamptz,
    remaining_active_seconds integer            CHECK (remaining_active_seconds IS NULL OR remaining_active_seconds >= 0),
    remaining_bytes          bigint             CHECK (remaining_bytes IS NULL OR remaining_bytes >= 0),
    fup_day                  date,                           -- local day of fup_bytes_today
    fup_bytes_today          bigint             NOT NULL DEFAULT 0,
    -- owned by exactly one subject: a member account or a guest access code
    CONSTRAINT entitlement_has_one_subject CHECK ((account_id IS NOT NULL) <> (access_code_id IS NOT NULL))
);
CREATE INDEX entitlements_account_idx ON entitlements (account_id, status, expires_at) WHERE account_id IS NOT NULL;
CREATE INDEX entitlements_code_idx    ON entitlements (access_code_id, status, expires_at) WHERE access_code_id IS NOT NULL;
CREATE UNIQUE INDEX entitlements_one_per_order ON entitlements (order_id) WHERE order_id IS NOT NULL;   -- never provision an order twice

CREATE TABLE devices (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id      uuid        NOT NULL REFERENCES tenants(id),
    mac            macaddr     NOT NULL,
    mac_randomised boolean     NOT NULL DEFAULT false,      -- locally administered bit set
    hostname       text,
    vendor         text,                                     -- from the MAC prefix
    os_family      text,                                     -- Android, iOS, Windows, macOS, Linux ...
    os_version     text,
    device_class   text,                                     -- phone, tablet, laptop, tv, console, iot, unknown
    user_agent     text,
    confidence     smallint    NOT NULL DEFAULT 0 CHECK (confidence BETWEEN 0 AND 100),
    first_seen_at  timestamptz NOT NULL DEFAULT now(),
    last_seen_at   timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, mac)
);

CREATE TABLE device_bindings (
    id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id       uuid        NOT NULL REFERENCES tenants(id),
    entitlement_id  uuid        NOT NULL REFERENCES entitlements(id),
    device_id       uuid        NOT NULL REFERENCES devices(id),
    bound_at        timestamptz NOT NULL DEFAULT now(),
    released_at     timestamptz
);
-- a device holds at most one live slot per entitlement
CREATE UNIQUE INDEX device_bindings_live ON device_bindings (entitlement_id, device_id) WHERE released_at IS NULL;

CREATE TABLE sessions (
    id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id       uuid        NOT NULL REFERENCES tenants(id),
    site_id         uuid        NOT NULL REFERENCES sites(id),
    entitlement_id  uuid        NOT NULL REFERENCES entitlements(id),
    device_id       uuid        NOT NULL REFERENCES devices(id),
    router_node_id  uuid        REFERENCES nodes(id),
    ap_node_id      uuid        REFERENCES nodes(id),
    ssid            text,
    band            text,
    vlan            integer,
    ip              inet,
    rssi_dbm        smallint,
    started_at      timestamptz NOT NULL DEFAULT now(),
    last_update_at  timestamptz NOT NULL DEFAULT now(),
    ended_at        timestamptz,
    bytes_in        bigint      NOT NULL DEFAULT 0 CHECK (bytes_in  >= 0),
    bytes_out       bigint      NOT NULL DEFAULT 0 CHECK (bytes_out >= 0),
    active_seconds  integer     NOT NULL DEFAULT 0 CHECK (active_seconds >= 0),
    end_reason      session_end_reason
);
CREATE INDEX sessions_site_time_idx   ON sessions (site_id, started_at DESC);
CREATE INDEX sessions_open_idx        ON sessions (site_id) WHERE ended_at IS NULL;
CREATE INDEX sessions_entitlement_idx ON sessions (entitlement_id);

-- ---------------------------------------------------------------------
-- Append-only ledgers
-- ---------------------------------------------------------------------
CREATE TABLE usage_ledger (
    id              bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id       uuid        NOT NULL REFERENCES tenants(id),
    entitlement_id  uuid        NOT NULL REFERENCES entitlements(id),
    session_id      uuid        REFERENCES sessions(id),
    ts              timestamptz NOT NULL DEFAULT now(),
    kind            text        NOT NULL CHECK (kind IN ('active_seconds', 'bytes', 'adjustment')),
    amount          bigint      NOT NULL,                   -- units consumed (negative = given back)
    note            text
);
CREATE INDEX usage_ledger_ent_idx ON usage_ledger (entitlement_id, ts);

CREATE TABLE wallet_ledger (
    id           bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id    uuid              NOT NULL REFERENCES tenants(id),
    account_id   uuid              NOT NULL REFERENCES accounts(id),
    ts           timestamptz       NOT NULL DEFAULT now(),
    kind         wallet_entry_kind NOT NULL,
    amount_kobo  bigint            NOT NULL CHECK (amount_kobo <> 0),   -- positive adds, negative spends
    order_id     uuid              REFERENCES orders(id),
    note         text
);
CREATE INDEX wallet_ledger_acct_idx ON wallet_ledger (account_id, ts);

CREATE VIEW wallet_balances AS
    SELECT tenant_id, account_id, sum(amount_kobo)::bigint AS balance_kobo
    FROM wallet_ledger GROUP BY tenant_id, account_id;

-- A wallet can never go below zero: checked when a spending row is inserted.
CREATE FUNCTION wallet_no_overdraft() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE bal bigint;
BEGIN
    PERFORM pg_advisory_xact_lock(hashtext(NEW.account_id::text));      -- serialise per account
    SELECT COALESCE(sum(amount_kobo), 0) INTO bal FROM wallet_ledger WHERE account_id = NEW.account_id;
    IF bal + NEW.amount_kobo < 0 THEN
        RAISE EXCEPTION 'wallet balance would go negative (balance %, change %)', bal, NEW.amount_kobo
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER wallet_no_overdraft BEFORE INSERT ON wallet_ledger
    FOR EACH ROW EXECUTE FUNCTION wallet_no_overdraft();

-- ---------------------------------------------------------------------
-- Security signals
-- ---------------------------------------------------------------------
CREATE TABLE tethering_flags (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   uuid        NOT NULL REFERENCES tenants(id),
    session_id  uuid        REFERENCES sessions(id),
    device_id   uuid        NOT NULL REFERENCES devices(id),
    ts          timestamptz NOT NULL DEFAULT now(),
    signal      text        NOT NULL CHECK (signal IN ('ttl', 'mixed_fingerprint', 'connection_count', 'slot_overflow')),
    score       smallint    NOT NULL CHECK (score BETWEEN 0 AND 100),
    action      text        NOT NULL CHECK (action IN ('none', 'warn', 'throttle', 'disconnect', 'review')),
    detail      jsonb       NOT NULL DEFAULT '{}'::jsonb
);

-- ---------------------------------------------------------------------
-- Time-series metrics (becomes a TimescaleDB hypertable when available)
-- ---------------------------------------------------------------------
CREATE TABLE metrics (
    ts         timestamptz      NOT NULL,
    tenant_id  uuid             NOT NULL REFERENCES tenants(id),
    site_id    uuid             NOT NULL REFERENCES sites(id),
    node_id    uuid             REFERENCES nodes(id),
    metric     text             NOT NULL,        -- e.g. throughput_down_bps, clients, rssi_dbm, radio_ccq
    value      double precision NOT NULL
);
CREATE INDEX metrics_lookup_idx ON metrics (site_id, metric, ts DESC);

-- ---------------------------------------------------------------------
-- Append-only enforcement
-- ---------------------------------------------------------------------
CREATE FUNCTION forbid_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION '% on % is not allowed: this table is append-only', TG_OP, TG_TABLE_NAME
        USING ERRCODE = 'insufficient_privilege';
END $$;

CREATE TRIGGER usage_ledger_append_only  BEFORE UPDATE OR DELETE ON usage_ledger  FOR EACH ROW EXECUTE FUNCTION forbid_mutation();
CREATE TRIGGER wallet_ledger_append_only BEFORE UPDATE OR DELETE ON wallet_ledger FOR EACH ROW EXECUTE FUNCTION forbid_mutation();

-- ---------------------------------------------------------------------
-- Hash-chained audit log
-- ---------------------------------------------------------------------
CREATE TABLE audit_log (
    seq         bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id   uuid        NOT NULL REFERENCES tenants(id),
    ts          timestamptz NOT NULL DEFAULT now(),
    actor_kind  text        NOT NULL CHECK (actor_kind IN ('admin', 'account', 'guest', 'system', 'node', 'paystack')),
    actor_id    text,
    action      text        NOT NULL,                      -- e.g. payment.confirmed, session.disconnect
    entity      text,
    entity_id   text,
    ip          inet,
    detail      jsonb       NOT NULL DEFAULT '{}'::jsonb,
    prev_hash   text        NOT NULL,
    hash        text        NOT NULL
);
CREATE INDEX audit_log_tenant_idx ON audit_log (tenant_id, seq);
CREATE INDEX audit_log_action_idx ON audit_log (tenant_id, action, ts DESC);

-- canonical text that the hash covers
CREATE FUNCTION audit_row_text(r audit_log) RETURNS text LANGUAGE sql STABLE AS $$
    SELECT concat_ws('|', r.prev_hash, r.tenant_id::text, extract(epoch FROM r.ts)::text,
                     r.actor_kind, coalesce(r.actor_id, ''), r.action, coalesce(r.entity, ''),
                     coalesce(r.entity_id, ''), coalesce(host(r.ip), ''), r.detail::text)
$$;

CREATE FUNCTION audit_chain_insert() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE last_hash text;
BEGIN
    -- one writer at a time per tenant keeps the chain linear
    PERFORM pg_advisory_xact_lock(hashtext('audit:' || NEW.tenant_id::text));
    SELECT hash INTO last_hash FROM audit_log WHERE tenant_id = NEW.tenant_id ORDER BY seq DESC LIMIT 1;
    NEW.prev_hash := coalesce(last_hash, repeat('0', 64));
    NEW.ts := now();                                       -- the clock is the database's, not the caller's
    NEW.hash := encode(sha256(convert_to(audit_row_text(NEW), 'UTF8')), 'hex');
    RETURN NEW;
END $$;
CREATE TRIGGER audit_chain_insert BEFORE INSERT ON audit_log FOR EACH ROW EXECUTE FUNCTION audit_chain_insert();
CREATE TRIGGER audit_append_only  BEFORE UPDATE OR DELETE ON audit_log FOR EACH ROW EXECUTE FUNCTION forbid_mutation();

-- Returns one row per broken link. An empty result means the chain is intact.
CREATE FUNCTION verify_audit_chain(p_tenant uuid)
RETURNS TABLE (bad_seq bigint, problem text) LANGUAGE plpgsql STABLE AS $$
DECLARE r audit_log; expected_prev text := repeat('0', 64);
BEGIN
    FOR r IN SELECT * FROM audit_log WHERE tenant_id = p_tenant ORDER BY seq LOOP
        IF r.prev_hash <> expected_prev THEN
            bad_seq := r.seq; problem := 'previous hash does not match'; RETURN NEXT;
        END IF;
        IF r.hash <> encode(sha256(convert_to(audit_row_text(r), 'UTF8')), 'hex') THEN
            bad_seq := r.seq; problem := 'row content does not match its hash'; RETURN NEXT;
        END IF;
        expected_prev := r.hash;
    END LOOP;
END $$;

-- ---------------------------------------------------------------------
-- Row-level security: every tenant sees only its own rows
-- ---------------------------------------------------------------------
CREATE FUNCTION current_tenant() RETURNS uuid LANGUAGE sql STABLE AS $$
    SELECT nullif(current_setting('app.tenant_id', true), '')::uuid
$$;

DO $$
DECLARE t text;
BEGIN
    FOREACH t IN ARRAY ARRAY[
        'sites', 'nodes', 'speed_profiles', 'plans', 'accounts', 'access_codes', 'consents',
        'admin_users', 'orders', 'payments', 'entitlements', 'devices', 'device_bindings',
        'sessions', 'usage_ledger', 'wallet_ledger', 'tethering_flags', 'metrics', 'audit_log']
    LOOP
        EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', t);
        EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', t);
        EXECUTE format('CREATE POLICY tenant_isolation ON %I USING (tenant_id = current_tenant()) WITH CHECK (tenant_id = current_tenant())', t);
    END LOOP;
END $$;

-- tenants: a tenant sees only itself
ALTER TABLE tenants ENABLE ROW LEVEL SECURITY;
ALTER TABLE tenants FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_self ON tenants USING (id = current_tenant()) WITH CHECK (id = current_tenant());

-- ---------------------------------------------------------------------
-- TimescaleDB (optional): turn metrics into a hypertable when the extension exists
-- ---------------------------------------------------------------------
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_available_extensions WHERE name = 'timescaledb') THEN
        CREATE EXTENSION IF NOT EXISTS timescaledb;
        PERFORM create_hypertable('metrics', 'ts', if_not_exists => true);
        RAISE NOTICE 'metrics is now a TimescaleDB hypertable';
    ELSE
        RAISE NOTICE 'TimescaleDB not installed: metrics stays a plain table (fine for development)';
    END IF;
END $$;

COMMIT;
