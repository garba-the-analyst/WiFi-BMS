-- Refunds: customer requests and admin decisions, one open record per order.

CREATE TABLE refunds (
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id     uuid        NOT NULL REFERENCES tenants(id),
    order_id      uuid        NOT NULL REFERENCES orders(id),
    amount_kobo   bigint      NOT NULL CHECK (amount_kobo > 0),
    status        text        NOT NULL CHECK (status IN ('requested', 'processed', 'declined', 'failed')),
    requested_by  text        NOT NULL CHECK (requested_by IN ('customer', 'admin')),
    reason        text,
    method        text        CHECK (method IN ('paystack', 'wallet')),   -- set when processed
    provider_ref  text,                                                 -- Paystack's refund id
    decided_by    uuid        REFERENCES admin_users(id),
    decided_at    timestamptz,
    note          text,                                                 -- admin's note or the failure message
    created_at    timestamptz NOT NULL DEFAULT now()
);
-- Backstop against refunding one order twice, whatever the application does.
CREATE UNIQUE INDEX refunds_one_open_per_order ON refunds (order_id) WHERE status IN ('requested', 'processed');
CREATE INDEX refunds_tenant_status_idx ON refunds (tenant_id, status, created_at DESC);

ALTER TABLE refunds ENABLE ROW LEVEL SECURITY;
ALTER TABLE refunds FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON refunds USING (tenant_id = current_tenant()) WITH CHECK (tenant_id = current_tenant());
GRANT SELECT, INSERT, UPDATE ON refunds TO platform_app;       -- never DELETE: refunds are financial records

-- An order refunded before it ever produced a plan has no account or code.
ALTER TABLE orders DROP CONSTRAINT order_has_one_subject;
ALTER TABLE orders ADD CONSTRAINT order_has_one_subject CHECK (
    (account_id IS NOT NULL AND access_code_id IS NULL) OR
    (account_id IS NULL AND access_code_id IS NOT NULL) OR
    (account_id IS NULL AND access_code_id IS NULL
        AND status IN ('created', 'pending', 'failed', 'expired', 'refund_flagged', 'refunded')
        AND phone IS NOT NULL)
);
