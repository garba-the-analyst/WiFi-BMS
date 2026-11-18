# Central deployment

Requirements: a VPS (1 vCPU / 1 GB is enough to start), a domain pointing at it (production: `wifi.waste2light.com`), Docker with Compose v2.

1. `cp .env.example .env`, set `PUBLIC_HOST`, two database passwords, and `PLATFORM_MASTER_KEY` (`openssl rand -hex 32`).
   **Back the master key up offline.**
2. `docker compose up -d db`
3. `docker compose --profile tools run --rm migrate` applies each migration once (recorded in `schema_migrations`),
   then creates the `platform_login` role the API uses. Re-run it after pulling new migrations.
4. Create your tenant (once, as the database owner):
   ```sql
   INSERT INTO tenants (name, slug) VALUES ('My Wi-Fi', 'mywifi');
   INSERT INTO sites (tenant_id, name, slug) SELECT id, 'Site 1', 'site-1' FROM tenants WHERE slug = 'mywifi';
   ```
   (`docker compose exec db psql -U owner wifibms`)
5. Edit `deploy/config.json` so `tenant` and `site` match what you created in step 4 (and set `brand`, `supportPhone`;
   add router addresses to `hotspotHosts` only if they are not private/`.lan`). Then
   `docker compose up -d --build api caddy`. The Caddy image builds the web app and obtains a TLS certificate automatically.
6. Create the first admin: `ADMIN_PASSWORD='long passphrase' docker compose run --rm -e ADMIN_PASSWORD api create-admin mywifi you@example.com owner`
7. Sign in (`POST /v1/t/mywifi/admin/login`), enrol 2-step verification (`/admin/totp/setup`, `/admin/totp/confirm`),
   set the Paystack key (`PUT /admin/secrets/paystack`), then create speed profiles and plans.
8. In the Paystack dashboard set the webhook URL to `https://<host>/v1/t/mywifi/webhooks/paystack`.
9. Create a node token per site (`POST /admin/node-tokens`) and follow `docs/site-setup.md`.

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
