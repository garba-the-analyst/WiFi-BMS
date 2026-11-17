-- =====================================================================
-- Schema tests. Run after migrations/0001_init.sql, as a superuser:
--     psql -v ON_ERROR_STOP=1 -f tests/schema_test.sql <database>
-- Each block ASSERTs one rule and prints a NOTICE when it holds. Any failure
-- aborts the run (ON_ERROR_STOP).
-- =====================================================================
\set ON_ERROR_STOP on

-- A non-owner role for the application, so row-level security applies to it
DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'app_user') THEN
        CREATE ROLE app_user LOGIN NOSUPERUSER NOBYPASSRLS;
    END IF;
END $$;
GRANT USAGE ON SCHEMA public TO app_user;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO app_user;

-- Two tenants, created by the superuser (provisioning is an operator task)
INSERT INTO tenants (id, name, slug) VALUES
  ('11111111-1111-1111-1111-111111111111', 'Tenant One', 'tenant-one'),
  ('22222222-2222-2222-2222-222222222222', 'Tenant Two', 'tenant-two');

-- ---------------- Test 1: tenant isolation ----------------
SET ROLE app_user;
SELECT set_config('app.tenant_id', '11111111-1111-1111-1111-111111111111', false);
INSERT INTO sites (id, tenant_id, name, slug) VALUES
  ('a0000000-0000-0000-0000-000000000001', '11111111-1111-1111-1111-111111111111', 'Site A', 'site-a');

SELECT set_config('app.tenant_id', '22222222-2222-2222-2222-222222222222', false);
DO $$
BEGIN
    ASSERT (SELECT count(*) FROM sites) = 0, 'tenant two must not see tenant one sites';
    ASSERT (SELECT count(*) FROM tenants) = 1, 'tenant two sees only itself';
    BEGIN
        INSERT INTO sites (tenant_id, name, slug) VALUES ('11111111-1111-1111-1111-111111111111', 'Sneaky', 'sneaky');
        RAISE EXCEPTION 'cross-tenant insert should have been refused';
    EXCEPTION WHEN insufficient_privilege THEN
        NULL;   -- expected: RLS WITH CHECK refuses the row
    END;
    RAISE NOTICE 'PASS 1: tenant isolation';
END $$;
SELECT set_config('app.tenant_id', '11111111-1111-1111-1111-111111111111', false);

-- ---------------- Test 2: plan budgets must match the plan kind ----------------
INSERT INTO speed_profiles (id, tenant_id, name, down_kbps, up_kbps) VALUES
  ('b0000000-0000-0000-0000-000000000001', '11111111-1111-1111-1111-111111111111', 'Standard 5M', 5000, 2000),
  ('b0000000-0000-0000-0000-000000000002', '11111111-1111-1111-1111-111111111111', 'Throttled 1M', 1000, 500);
DO $$
BEGIN
    -- a time plan carrying a data budget is invalid
    BEGIN
        INSERT INTO plans (tenant_id, name, kind, price_kobo, active_seconds, data_bytes, valid_seconds, speed_profile_id)
        VALUES ('11111111-1111-1111-1111-111111111111', 'Bad time plan', 'time', 10000, 3600, 1000000, 86400, 'b0000000-0000-0000-0000-000000000001');
        RAISE EXCEPTION 'mismatched plan should have been refused';
    EXCEPTION WHEN check_violation THEN NULL; END;
    -- a plan with a fair-use cap but no throttled profile is invalid
    BEGIN
        INSERT INTO plans (tenant_id, name, kind, price_kobo, active_seconds, valid_seconds, speed_profile_id, fup_daily_bytes)
        VALUES ('11111111-1111-1111-1111-111111111111', 'Bad fup plan', 'time', 10000, 3600, 86400, 'b0000000-0000-0000-0000-000000000001', 5000000000);
        RAISE EXCEPTION 'fup without profile should have been refused';
    EXCEPTION WHEN check_violation THEN NULL; END;
    -- good plans
    INSERT INTO plans (id, tenant_id, name, kind, price_kobo, active_seconds, valid_seconds, speed_profile_id) VALUES
      ('c0000000-0000-0000-0000-000000000001', '11111111-1111-1111-1111-111111111111', 'Test hourly', 'time', 10000, 3600, 86400, 'b0000000-0000-0000-0000-000000000001');
    INSERT INTO plans (tenant_id, name, kind, price_kobo, data_bytes, valid_seconds, speed_profile_id) VALUES
      ('11111111-1111-1111-1111-111111111111', 'Test data', 'data', 50000, 1073741824, 2592000, 'b0000000-0000-0000-0000-000000000001');
    INSERT INTO plans (tenant_id, name, kind, price_kobo, active_seconds, data_bytes, valid_seconds, device_limit, speed_profile_id, fup_daily_bytes, fup_speed_profile_id) VALUES
      ('11111111-1111-1111-1111-111111111111', 'Test hybrid', 'hybrid', 90000, 86400, 5368709120, 604800, 3, 'b0000000-0000-0000-0000-000000000001', 1073741824, 'b0000000-0000-0000-0000-000000000002');
    RAISE NOTICE 'PASS 2: plan constraints';
END $$;

-- ---------------- Test 3: orders, idempotent payments, one entitlement per order ----------------
INSERT INTO access_codes (id, tenant_id, code, phone) VALUES
  ('d0000000-0000-0000-0000-000000000001', '11111111-1111-1111-1111-111111111111', 'ABCD2345', '+2348000000000');
INSERT INTO orders (id, tenant_id, site_id, plan_id, reference, amount_kobo, status, access_code_id, phone) VALUES
  ('e0000000-0000-0000-0000-000000000001', '11111111-1111-1111-1111-111111111111', 'a0000000-0000-0000-0000-000000000001',
   'c0000000-0000-0000-0000-000000000001', 'ref-001', 10000, 'paid', 'd0000000-0000-0000-0000-000000000001', '+2348000000000');
DO $$
BEGIN
    INSERT INTO payments (tenant_id, order_id, provider_event_id, event_type, amount_kobo, raw_event)
    VALUES ('11111111-1111-1111-1111-111111111111', 'e0000000-0000-0000-0000-000000000001', 'txn-9001', 'charge.success', 10000, '{}');
    -- Paystack resends the same webhook: must be refused, not duplicated
    BEGIN
        INSERT INTO payments (tenant_id, order_id, provider_event_id, event_type, amount_kobo, raw_event)
        VALUES ('11111111-1111-1111-1111-111111111111', 'e0000000-0000-0000-0000-000000000001', 'txn-9001', 'charge.success', 10000, '{}');
        RAISE EXCEPTION 'duplicate webhook should have been refused';
    EXCEPTION WHEN unique_violation THEN NULL; END;
    -- a duplicate order reference is refused
    BEGIN
        INSERT INTO orders (tenant_id, site_id, plan_id, reference, amount_kobo, status, phone)
        VALUES ('11111111-1111-1111-1111-111111111111', 'a0000000-0000-0000-0000-000000000001',
                'c0000000-0000-0000-0000-000000000001', 'ref-001', 10000, 'created', '+2348000000000');
        RAISE EXCEPTION 'duplicate reference should have been refused';
    EXCEPTION WHEN unique_violation THEN NULL; END;
    -- an entitlement per order, only once
    INSERT INTO entitlements (id, tenant_id, order_id, plan_id, plan_snapshot, access_code_id, remaining_active_seconds)
    VALUES ('f0000000-0000-0000-0000-000000000001', '11111111-1111-1111-1111-111111111111',
            'e0000000-0000-0000-0000-000000000001', 'c0000000-0000-0000-0000-000000000001', '{}', 'd0000000-0000-0000-0000-000000000001', 3600);
    BEGIN
        INSERT INTO entitlements (tenant_id, order_id, plan_id, plan_snapshot, access_code_id, remaining_active_seconds)
        VALUES ('11111111-1111-1111-1111-111111111111', 'e0000000-0000-0000-0000-000000000001',
                'c0000000-0000-0000-0000-000000000001', '{}', 'd0000000-0000-0000-0000-000000000001', 3600);
        RAISE EXCEPTION 'second entitlement for one order should have been refused';
    EXCEPTION WHEN unique_violation THEN NULL; END;
    -- an entitlement must belong to exactly one subject
    BEGIN
        INSERT INTO entitlements (tenant_id, plan_id, plan_snapshot) VALUES
          ('11111111-1111-1111-1111-111111111111', 'c0000000-0000-0000-0000-000000000001', '{}');
        RAISE EXCEPTION 'entitlement with no owner should have been refused';
    EXCEPTION WHEN check_violation THEN NULL; END;
    RAISE NOTICE 'PASS 3: orders, payments and entitlements are idempotent';
END $$;

-- ---------------- Test 4: append-only usage ledger ----------------
INSERT INTO usage_ledger (tenant_id, entitlement_id, kind, amount)
VALUES ('11111111-1111-1111-1111-111111111111', 'f0000000-0000-0000-0000-000000000001', 'active_seconds', 60);
DO $$
BEGIN
    BEGIN
        UPDATE usage_ledger SET amount = 1;
        RAISE EXCEPTION 'ledger update should have been refused';
    EXCEPTION WHEN insufficient_privilege THEN NULL; END;
    BEGIN
        DELETE FROM usage_ledger;
        RAISE EXCEPTION 'ledger delete should have been refused';
    EXCEPTION WHEN insufficient_privilege THEN NULL; END;
    RAISE NOTICE 'PASS 4: usage ledger is append-only';
END $$;

-- ---------------- Test 5: wallet balance never goes negative ----------------
INSERT INTO accounts (id, tenant_id, phone) VALUES
  ('90000000-0000-0000-0000-000000000001', '11111111-1111-1111-1111-111111111111', '+2348011111111');
INSERT INTO wallet_ledger (tenant_id, account_id, kind, amount_kobo)
VALUES ('11111111-1111-1111-1111-111111111111', '90000000-0000-0000-0000-000000000001', 'topup', 50000);
DO $$
BEGIN
    INSERT INTO wallet_ledger (tenant_id, account_id, kind, amount_kobo)
    VALUES ('11111111-1111-1111-1111-111111111111', '90000000-0000-0000-0000-000000000001', 'purchase', -20000);
    ASSERT (SELECT balance_kobo FROM wallet_balances WHERE account_id = '90000000-0000-0000-0000-000000000001') = 30000, 'balance should be 30000';
    BEGIN
        INSERT INTO wallet_ledger (tenant_id, account_id, kind, amount_kobo)
        VALUES ('11111111-1111-1111-1111-111111111111', '90000000-0000-0000-0000-000000000001', 'purchase', -30001);
        RAISE EXCEPTION 'overdraft should have been refused';
    EXCEPTION WHEN check_violation THEN NULL; END;
    RAISE NOTICE 'PASS 5: wallet cannot overdraw';
END $$;

-- ---------------- Test 6: audit chain is linked and tamper-evident ----------------
INSERT INTO audit_log (tenant_id, actor_kind, actor_id, action, entity, entity_id, detail) VALUES
  ('11111111-1111-1111-1111-111111111111', 'system', 'test', 'payment.confirmed', 'order', 'ref-001', '{"amount_kobo": 10000}');
INSERT INTO audit_log (tenant_id, actor_kind, actor_id, action, entity, entity_id, detail) VALUES
  ('11111111-1111-1111-1111-111111111111', 'admin', 'owner@example.com', 'session.disconnect', 'session', 's-1', '{"reason": "tethering"}');
INSERT INTO audit_log (tenant_id, actor_kind, action) VALUES
  ('11111111-1111-1111-1111-111111111111', 'system', 'plan.created');
DO $$
BEGIN
    ASSERT (SELECT count(*) FROM verify_audit_chain('11111111-1111-1111-1111-111111111111')) = 0, 'fresh chain should verify clean';
    ASSERT (SELECT count(DISTINCT prev_hash) FROM audit_log) = 3, 'each row links to a different previous hash';
    BEGIN
        UPDATE audit_log SET action = 'nothing.happened';
        RAISE EXCEPTION 'audit update should have been refused';
    EXCEPTION WHEN insufficient_privilege THEN NULL; END;
    BEGIN
        DELETE FROM audit_log;
        RAISE EXCEPTION 'audit delete should have been refused';
    EXCEPTION WHEN insufficient_privilege THEN NULL; END;
    RAISE NOTICE 'PASS 6a: audit log is linked and append-only for the application';
END $$;

-- A superuser can bypass triggers; the chain must still reveal the tampering
RESET ROLE;
SET session_replication_role = replica;      -- disables the append-only trigger, as an attacker with DB superuser might
UPDATE audit_log SET detail = '{"amount_kobo": 1}' WHERE action = 'payment.confirmed';
SET session_replication_role = origin;
SELECT set_config('app.tenant_id', '11111111-1111-1111-1111-111111111111', false);
DO $$
DECLARE bad bigint;
BEGIN
    SELECT count(*) INTO bad FROM verify_audit_chain('11111111-1111-1111-1111-111111111111');
    ASSERT bad >= 1, 'tampering must be detected by verify_audit_chain';
    RAISE NOTICE 'PASS 6b: tampering with an audit row is detected (% problem(s) reported)', bad;
END $$;

-- ---------------- Test 7: a device holds one live slot per entitlement ----------------
INSERT INTO devices (id, tenant_id, mac, os_family, device_class) VALUES
  ('aa000000-0000-0000-0000-000000000001', '11111111-1111-1111-1111-111111111111', 'aa:bb:cc:dd:ee:01', 'Android', 'phone');
INSERT INTO device_bindings (tenant_id, entitlement_id, device_id) VALUES
  ('11111111-1111-1111-1111-111111111111', 'f0000000-0000-0000-0000-000000000001', 'aa000000-0000-0000-0000-000000000001');
DO $$
BEGIN
    BEGIN
        INSERT INTO device_bindings (tenant_id, entitlement_id, device_id) VALUES
          ('11111111-1111-1111-1111-111111111111', 'f0000000-0000-0000-0000-000000000001', 'aa000000-0000-0000-0000-000000000001');
        RAISE EXCEPTION 'duplicate live binding should have been refused';
    EXCEPTION WHEN unique_violation THEN NULL; END;
    RAISE NOTICE 'PASS 7: device bindings';
END $$;

\echo 'ALL SCHEMA TESTS PASSED'
