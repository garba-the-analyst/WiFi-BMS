-- 0006: live view of the network hardware (routers, access points, CPE radios, switches):
-- their health, signal and the devices connected to each. Filled by the site agent.
-- Run after 0005, as the database owner (deploy/migrate.sh does this).

CREATE TABLE net_devices (
    id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id       uuid        NOT NULL REFERENCES tenants(id),
    site_id         uuid        NOT NULL REFERENCES sites(id),
    dev_key         text        NOT NULL,                    -- stable id chosen by the agent (MAC address or configured name)
    name            text        NOT NULL,
    kind            text        NOT NULL DEFAULT 'other' CHECK (kind IN ('router', 'ap', 'cpe', 'switch', 'controller', 'other')),
    model           text,
    mac             text,
    ip              text,
    firmware        text,
    peer            text,                                    -- what it is linked to: the other end of a radio link, or the AP's uplink
    status          text        NOT NULL DEFAULT 'online' CHECK (status IN ('online', 'degraded', 'offline')),
    reasons         jsonb       NOT NULL DEFAULT '[]'::jsonb,    -- why it is degraded or offline, in words
    alert_state     text        NOT NULL DEFAULT 'ok' CHECK (alert_state IN ('ok', 'degraded', 'offline')),   -- what staff were last told
    reachable       boolean     NOT NULL DEFAULT true,       -- could the site agent reach it on the last round
    uptime_s        bigint,
    cpu_pct         real,
    mem_pct         real,
    signal_dbm      real,                                    -- radio link signal (CPE) as seen by this device
    noise_dbm       real,
    link_quality_pct real,
    tx_mbps         real,
    rx_mbps         real,
    channel         text,
    util_pct        real,                                    -- channel / link utilisation
    latency_ms      real,
    loss_pct        real,
    clients_count   integer     NOT NULL DEFAULT 0,
    details         jsonb       NOT NULL DEFAULT '{}'::jsonb,
    first_seen_at   timestamptz NOT NULL DEFAULT now(),
    last_seen_at    timestamptz NOT NULL DEFAULT now(),      -- last time it answered
    reported_at     timestamptz NOT NULL DEFAULT now(),      -- last time the site agent mentioned it
    UNIQUE (tenant_id, site_id, dev_key)
);
CREATE INDEX net_devices_site_idx ON net_devices (tenant_id, site_id, kind);

CREATE TABLE net_device_samples (
    ts          timestamptz NOT NULL DEFAULT now(),
    tenant_id   uuid        NOT NULL REFERENCES tenants(id),
    device_id   uuid        NOT NULL REFERENCES net_devices(id) ON DELETE CASCADE,
    signal_dbm  real,
    cpu_pct     real,
    mem_pct     real,
    util_pct    real,
    latency_ms  real,
    loss_pct    real,
    tx_mbps     real,
    rx_mbps     real,
    clients     integer
);
CREATE INDEX net_device_samples_idx ON net_device_samples (device_id, ts DESC);

-- Who is connected to each device right now. Replaced on every report.
CREATE TABLE net_clients (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id   uuid        NOT NULL REFERENCES tenants(id),
    device_id   uuid        NOT NULL REFERENCES net_devices(id) ON DELETE CASCADE,
    mac         macaddr     NOT NULL,
    hostname    text,
    ip          text,
    ssid        text,
    signal_dbm  real,
    connected_s bigint,
    rx_bytes    bigint,
    tx_bytes    bigint,
    seen_at     timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX net_clients_device_idx ON net_clients (device_id);
CREATE INDEX net_clients_mac_idx ON net_clients (tenant_id, mac);

DO $$
DECLARE t text;
BEGIN
  FOREACH t IN ARRAY ARRAY['net_devices', 'net_device_samples', 'net_clients']
  LOOP
    EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', t);
    EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', t);
    EXECUTE format('CREATE POLICY tenant_isolation ON %I USING (tenant_id = current_tenant()) WITH CHECK (tenant_id = current_tenant())', t);
  END LOOP;
END $$;
GRANT SELECT, INSERT, UPDATE, DELETE ON net_devices, net_device_samples, net_clients TO platform_app;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO platform_app;
