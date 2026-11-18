# Central deployment

Requirements: a VPS (1 vCPU / 1 GB is enough to start), a domain pointing at it (production: `wifi.waste2light.com`), Docker with Compose v2.

1. `cp .env.example .env`, set `PUBLIC_HOST`, two database passwords, and `PLATFORM_MASTER_KEY` (`openssl rand -hex 32`).
   **Back the master key up offline.**
2. `docker compose up -d db`
3. `docker compose --profile tools run --rm migrate` applies each migration once (recorded in `schema_migrations`),
   then creates the `platform_login` role the API uses. Re-run it after pulling new migrations.
4. Create your establishment (once per business; this is how one install serves several):
   `docker compose run --rm api create-tenant mywifi "My Wi-Fi" site-1 "Main site"`
5. Edit `deploy/config.json` so `tenant` and `site` match step 4. Name, tagline, colour and support phone are set later in
   **Settings → Business details**, so one build serves every establishment. Then
   `docker compose up -d --build api caddy`. The Caddy image builds the web app and obtains a TLS certificate automatically.
6. Create the first system admin: `ADMIN_PASSWORD='long passphrase' docker compose run --rm -e ADMIN_PASSWORD api create-admin mywifi you@example.com system_admin`
   (roles: `system_admin`, `ceo`, `admin`, `customer_rep`; the others are created in the **Staff** page).
7. Sign in at `/admin`. You are asked to turn on 2-step verification first. Then add the Paystack key in Settings, create plans, and
   add your CEO and staff.
8. In the Paystack dashboard set the webhook URL to `https://<host>/v1/t/mywifi/webhooks/paystack`.
9. Create a site key per site (Settings → Site agent keys) and follow `docs/site-setup.md`.

### Second establishment
Repeat step 4 with a new slug, create its system admin (step 6), and point a second host or config at it. Data is separated by
row-level security: staff, customers, plans, vouchers and logs of one establishment are never visible to another.

### 2-step verification
Every staff account must use one method: an authenticator app (always available), an emailed code, or an SMS code. Email and SMS
are offered only when the server is configured for them (otherwise the app is the only option):

| Variable | Purpose |
|---|---|
| `SMTP_URL`, `MAIL_FROM` | email codes, e.g. `smtps://user:pass@smtp.example.com:465` |
| `TERMII_API_KEY`, `TERMII_SENDER_ID` | SMS codes through Termii (written from their documentation; send a test code to your own phone before relying on it) |
| `REQUIRE_2FA=0` | development only: allows sign-in without 2-step verification |

A staff member who loses their phone is reset by their manager (Staff → Manage → Reset 2-step) and enrols again.
Upgrading from an earlier version: existing accounts keep working, the old owner becomes `system_admin`, and each person is asked
to enrol 2-step verification at their next sign-in.

### The error log
**Error log** in the admin area (visible to every staff role) collects problems from the API, database, payments, SMS, email,
the web app, hardware (a site agent that goes silent for 5 minutes, and its recovery), RADIUS and the site agent itself. Each entry
carries a "check first" hint, the request id, details and a stack where there is one; repeats are grouped. Entries are kept 90 days.
Known limit: a failure that cannot be tied to one establishment (a request with no tenant in its address) appears in the server's
own log (`docker compose logs api`) only.

## Backups
`docker compose exec db pg_dump -U owner wifibms | gzip > backup-$(date +%F).sql.gz` nightly, copied off the server.
Test a restore. The `audit_log` hash chain survives a restore; verify with `/admin/audit/verify`.

## Upgrades
`git pull && docker compose build api && docker compose --profile tools run --rm migrate && docker compose up -d api`.

## One host, one certificate

Everything is served from `https://wifi.waste2light.com`:

| URL | What |
|---|---|
| `/` , `/order/…`, `/member…`, `/connect` | customer portal (captive portal landing, plans, wallet, refund request) |
| `/admin` | staff back office, same web app. Every call it makes needs an `at_` token and the role for that action |
| `/v1/*`, `/healthz` | API (Caddy proxies only these to the `api` container) |

There is no admin subdomain. Hiding `/admin` is not the protection; the sign-in, roles and 2-step verification are.

### Cloudflare
1. DNS: `A  wifi  →  <VPS IP>`. Proxied (orange cloud) is fine.
2. SSL/TLS mode: **Full (strict)**. Caddy holds a valid certificate, so strict works. (Until the first certificate is issued,
   set the record to DNS-only for a minute if validation fails.)
3. Caddy trusts Cloudflare's address ranges (`deploy/Caddyfile`) and reads the visitor's address from `CF-Connecting-IP`.
   Re-check the list at <https://www.cloudflare.com/ips/> from time to time. Without this the rate limits would see
   Cloudflare, not the customer.
4. Firewall the VPS to ports 80/443 (and SSH from your own address).

### The router
- Upload `deploy/routeros-login.html` as the hotspot's `login.html` (set `site=` per router). It sends the customer to the portal;
  the portal then logs them in with a one-time ticket by GET to the router's `link-login-only` address.
- Walled garden: allow `wifi.waste2light.com`, `*.paystack.com`, `*.paystack.co`, `js.paystack.co`, `checkout.paystack.com`
  so customers can reach the portal and pay before they are online. Add Cloudflare's challenge hosts only if you enable its challenges on the site.
- Change `terms.html` (`web/public/terms.html`) to your real terms before launch; have them reviewed for Nigerian law.
