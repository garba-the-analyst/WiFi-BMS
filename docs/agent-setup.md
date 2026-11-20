# Agent runbook: set up WiFi-BMS for real on ONE machine (production, no VPS)

You are installing WiFi-BMS on the machine you run on, which is the site machine, for live customers. Read `README.md`, `docs/single-box.md`,
`docs/installation.md`, `docs/site-setup.md`, `docs/api.md` first. Where this runbook and the code disagree, trust the code and tell the human.

## Rules
1. **Secrets** come only from `~/wifi-bms-secrets.env` (mode 600). Never print them, log them, commit them or put them in command lines that are echoed.
2. **Ask before anything destructive or irreversible**: deleting volumes, `docker compose down -v`, resetting or rebooting the router, changing the machine's own network or firewall (never cut off SSH), enabling `ufw` before allowing SSH.
3. **Router:** it was already reset by the human to the base config (identity wifibms-router, `192.168.88.1/24` on ether2, vlan10 `10.10.0.1/22`, masquerade, DHCP client on ether1, and the two WAN input rules) and is reachable by IP at `192.168.88.1` (Winbox MAC connect does not work from this PC). Apply router changes over SSH one block at a time, show each block first, verify after each, never touch ether2, its address or the human's connection, never reset or reboot the router. **The Starlink cable stays unplugged from ether1 until the human says otherwise.**
4. **Payments:** only a Paystack **test** key. The human swaps in the live key after a successful test purchase. Never ask for or use a live key.
5. Work in order, verify each phase with a command, record evidence in `SETUP_LOG.md` (excluded from git). A failing check: diagnose and retry up to 3 times, then stop and report.
6. Do not change application code unless a real bug blocks you; then a separate commit with a test, and tell the human.
7. Physical or interactive steps are the human's (wiring, powering, Cloudflare dashboard clicks, scanning the 2-step QR code, phone tests, adopting APs). Stop, tell them exactly what to do, wait.

## Inputs (in `~/wifi-bms-secrets.env`)
`PUBLIC_HOST=wifi.waste2light.com`, `TENANT=waste2light`, `SITE=site-1`, `BRAND`, `ADMIN_EMAIL`, `ADMIN_PASSWORD`, `PAYSTACK_TEST_KEY`, `ROUTER_SSH=admin@192.168.88.1`, `ROUTER_PASS`.
Generate yourself and store (never print): `RADIUS_SECRET`, `LOCAL_KEY` (`openssl rand -hex 24`), `ROUTER_REST_PASS`, DB passwords, `PLATFORM_MASTER_KEY` (`openssl rand -hex 32`; tell the human once to back it up offline).
Later from the human: `TUNNEL_TOKEN`, `OMADA_*`.

## Phase 1: Inspect and prepare the machine
Report OS, CPU, RAM, free disk, IP addresses. Install what is missing: Docker CE with Compose v2.24+, git, `freeradius freeradius-rest freeradius-utils iputils-ping snmp sshpass ufw`, Rust and Node only if you need to build the agent or run tests.
Set the machine's address to `192.168.88.2/24` (gateway `192.168.88.1`) only after asking the human; it must not break the current session.
Allow SSH in ufw before enabling it; then `ufw allow from 192.168.88.1 to any port 1812,1813 proto udp`.

## Phase 2: Platform on this machine (single-box mode)
Follow `docs/single-box.md` steps 2 to 4 exactly:
1. `.env` from `.env.example` with the generated values, `PUBLIC_HOST`, `TUNNEL_TOKEN` empty, `REQUIRE_2FA=0` **only for bootstrap**. `deploy/config.json` tenant/site/brand.
2. `docker compose up -d db`, run migrations, `create-tenant`, `create-admin` (system_admin), then
   `docker compose -f docker-compose.yml -f docker-compose.tunnel.yml up -d --build api caddy`. The tunnel is **off**, so nothing is reachable from the internet yet.
3. Verify: all containers healthy, `curl -s http://127.0.0.1:8080/healthz`, migrations recorded in `schema_migrations`.
4. Seed through the admin API: Paystack test key; plans (1 hour single device, 1 day single device, 1 week 3 devices, 1 month 5 devices, 5 GB data; default 2 Mbps); a **site agent key** for `$SITE` (store `NODE_TOKEN`, shown once); a customer-rep and an admin test account to confirm role limits; one manual voucher and one special voucher for testing.
5. **Lock down:** remove `REQUIRE_2FA=0` from `.env` and `docker compose ... up -d api` so 2-step verification is required again. Ask the human to create the Cloudflare tunnel per `docs/single-box.md` step 1 and put `TUNNEL_TOKEN` in the secrets file and `.env`. Then `docker compose -f docker-compose.yml -f docker-compose.tunnel.yml up -d` and verify from outside the LAN path: `curl -sI https://$PUBLIC_HOST/healthz` returns 200 and `https://$PUBLIC_HOST/admin` loads.
6. Tell the human to sign in at `/admin` and enrol 2-step verification, and to add the Paystack webhook `https://$PUBLIC_HOST/v1/t/$TENANT/webhooks/paystack` in the Paystack dashboard. Wait for confirmation.

## Phase 3: Router (over SSH, blocks one at a time)
1. Customer isolation: forward drop `in-interface=vlan10 dst-address=192.168.88.0/24`; input drop `in-interface=vlan10 protocol=tcp dst-port=22,23,8291,8728,8729`.
2. Services: disable telnet, ftp, www, api, api-ssl; winbox, ssh, www-ssl `available-from=192.168.88.0/24`.
3. Hotspot on vlan10: pool `10.10.0.10-10.10.3.250`, DHCP server on vlan10 (network `10.10.0.0/22`, gateway `10.10.0.1`), profile `hsprof1` (`login-by=http-pap use-radius=yes radius-accounting=yes radius-interim-update=1m hotspot-address=10.10.0.1`), `hotspot1` on vlan10.
4. RADIUS to `192.168.88.2` with `RADIUS_SECRET`, `/radius incoming accept=yes port=3799`, walled garden (`wifi.waste2light.com`, `*.paystack.com`, `*.paystack.co`, `js.paystack.co`, `checkout.paystack.com`) using an edited **copy** of `deploy/routeros-hotspot.rsc` (never commit the secret). Upload `deploy/routeros-login.html` as `hotspot/login.html` with `site=$SITE`.
5. Read-only monitoring user: group `readonly-api` (`read,api,rest-api`), user `agent` with `ROUTER_REST_PASS`; enable `www-ssl`.
Finish with `/ip firewall filter print`, `/ip service print`, `/ip hotspot print`, `/radius print`, `/ip hotspot walled-garden print` and show the full output to the human.

## Phase 4: Site agent and FreeRADIUS (this machine)
Per `docs/installation.md` Part 3.1 to 3.4, with `CENTRAL_URL=http://127.0.0.1:8080`, `NODE_TOKEN`, `LOCAL_KEY`, `COA_ADDR=192.168.88.1:3799`, `COA_SECRET=$RADIUS_SECRET`, `RADCLIENT_BIN=radclient`; FreeRADIUS `rest` module with `LOCAL_KEY`, `clients.conf` entry for `192.168.88.1` with `RADIUS_SECRET`.
Verify: agent healthy, the site shows online in admin, `freeradius -XC` clean, then service enabled. Prove the path without the router: `radclient` Access-Request with the test voucher (expect Accept), a bad code (Reject), Accounting Start/Interim/Stop, and a revoke producing a Disconnect packet at `COA_ADDR` (watch with tcpdump).

## Phase 5: Devices page
`/etc/wifi-bms/devices.json` from `deploy/devices.example.json`, keeping only what exists (routeros with `agent`/`ROUTER_REST_PASS`, ping for each AP and CPE). Omada only if the human has installed the controller and given `OMADA_*`. Mode `640 root:wifibms`, `DEVICES_FILE` in the agent env, restart. Verify the Devices page lists the router and no `hardware.collector_failed` is raised.

## Phase 6: Go live (with the human)
1. Human plugs Starlink into ether1. Verify `/ip dhcp-client print` is `bound`, the router can `/ping 8.8.8.8`, DNS works.
2. Human joins the Wi-Fi with a phone. Confirm: `10.10.x.x` address, login page redirects to the portal, entering the test voucher gives internet, the session shows in Customers, bytes are counted, revoking the voucher disconnects the phone within about a minute.
3. Test purchase with the Paystack test card; the order turns paid; then the human pastes the live key in admin Settings and makes one real small purchase and refunds it.
4. Install the nightly database backup from `docs/single-box.md` and run it once; confirm the file exists.

## Final report
A table of phase, what was done, evidence, pass or fail; versions; every file created or edited with its mode; credentials by name only and where stored; deviations from the docs; open items for the human; and a one-page "how to operate": start, stop, logs, upgrade (`git pull` then the compose command), restore from backup. Commit only intentional non-secret changes and push.
