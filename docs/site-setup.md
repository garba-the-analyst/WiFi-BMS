# Site setup

1. Admin panel: create a node for the site, copy its `nt_` token once.
2. Install `site-agent` (systemd unit in `deploy/`), fill `/etc/wifi-bms/site-agent.env`.
3. Install FreeRADIUS, apply `deploy/freeradius-rest.conf`.
4. Run `deploy/routeros-hotspot.rsc` on the router (adjust).
5. Test: `curl -H 'X-Agent-Key: ...' localhost:8081/healthz`, then connect with a test code.

Needs hardware validation: FreeRADIUS rlm_rest JSON shape, radclient CoA/Disconnect,
RouterOS REST. The agent does not yet send TTL/connection observations, so tether
detection currently relies on device slots only.
