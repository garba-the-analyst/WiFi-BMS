# Central deployment

Requirements: a VPS (1 vCPU / 1 GB is enough to start), a domain pointing at it, Docker with Compose v2.

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
5. `docker compose up -d api caddy`. Caddy obtains a TLS certificate automatically.
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
