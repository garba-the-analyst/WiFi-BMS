# Running everything on the site machine (no VPS)

One always-on machine at the site runs the whole platform: PostgreSQL, the API, the web app, the site agent, FreeRADIUS and
(optionally) the Omada controller. Starlink puts you behind carrier-grade NAT, so nothing from the internet can connect in.
A **Cloudflare Tunnel** solves that: a small container (`cloudflared`) opens an *outbound* connection to Cloudflare, and Cloudflare
forwards `https://wifi.waste2light.com` through it to Caddy. No ports are opened on the machine or the router.

```
customer phone ──> Wi-Fi AP ──> MikroTik (hotspot) ──RADIUS──> FreeRADIUS ──> site agent ──http://127.0.0.1:8080──> API ──> PostgreSQL
                                     │                                                                              ▲
                                     └─ walled garden: wifi.waste2light.com ──> Cloudflare ──tunnel──> cloudflared ──> Caddy ──> API / web app
Paystack webhook ──> Cloudflare ──tunnel──> Caddy ──> API
```

## What you need
- A Linux machine that stays on (Ubuntu Server 24.04), 4 GB RAM or more, an SSD, on the UPS, static address `192.168.88.2`.
- Docker with **Compose v2.24 or newer** (`docker compose version`).
- `waste2light.com` on Cloudflare (already the plan), and a free Cloudflare Zero Trust account (it may ask for a card for verification; the plan stays free).

## Set up
1. **Create the tunnel.** Cloudflare dashboard → **Zero Trust → Networks → Tunnels** (called *Connectors* in newer dashboards) → **Create a tunnel** →
   type **Cloudflared** → name `wifi-site` → copy the **token** shown. On the **Public hostname** step add: Subdomain `wifi`, Domain `waste2light.com`,
   Service type `HTTP`, URL `caddy:80`. Save. (If an `A` record named `wifi` already exists in DNS, delete it first; the tunnel creates a CNAME.)
2. In the repo: `cp .env.example .env`. Set `PUBLIC_HOST=wifi.waste2light.com`, two long random database passwords, `PLATFORM_MASTER_KEY` (`openssl rand -hex 32`, **back it up offline**) and `TUNNEL_TOKEN`.
3. **Bootstrap with the tunnel off** (so the admin is never exposed before 2-step verification exists). Edit `.env`: add `REQUIRE_2FA=0` and leave `TUNNEL_TOKEN` empty for now, then:
   ```
   docker compose up -d db
   docker compose --profile tools run --rm migrate
   docker compose run --rm api create-tenant waste2light "Waste2Light Wi-Fi" site-1 "Main site"
   ADMIN_PASSWORD='long passphrase' docker compose run --rm -e ADMIN_PASSWORD api create-admin waste2light you@example.com system_admin
   docker compose -f docker-compose.yml -f docker-compose.tunnel.yml up -d --build api caddy
   ```
   (Without a token the `cloudflared` service is not started by `up ... api caddy`.) Seed plans, the site agent key and your Paystack **test** key now (docs/installation.md Part 2 step 5), or let the agent do it through the API.
4. **Lock it down and go public.** Remove `REQUIRE_2FA=0` from `.env`, put the real `TUNNEL_TOKEN` in, then
   `docker compose -f docker-compose.yml -f docker-compose.tunnel.yml up -d --build`. Open `https://wifi.waste2light.com/admin`: you are asked to enrol 2-step verification at your next sign-in (authenticator app).
5. Paystack → Settings → API Keys & Webhooks → webhook `https://wifi.waste2light.com/v1/t/waste2light/webhooks/paystack`. Test with the **test** key; swap in the live key under admin → Settings only after a successful test purchase.
6. Site agent on the same machine: `CENTRAL_URL=http://127.0.0.1:8080` (the agent allows plain HTTP to localhost only). Everything else as in docs/installation.md Part 3.

Start the stack on boot: Docker restarts the containers (`restart: unless-stopped`); the systemd units for the agent and FreeRADIUS are enabled in Part 3.

## Backups (do this before opening to customers)
One machine means one point of failure. Nightly dump, plus a copy off the machine:
```
sudo mkdir -p /var/backups/wifibms
echo '15 2 * * * root cd /home/<you>/WiFi-BMS && docker compose exec -T db pg_dump -U owner wifibms | gzip > /var/backups/wifibms/$(date +\%F).sql.gz && find /var/backups/wifibms -mtime +30 -delete' | sudo tee /etc/cron.d/wifibms-backup
```
Copy `/var/backups/wifibms` to a USB drive or cloud storage regularly (for example with `rclone`), and keep `PLATFORM_MASTER_KEY` and `.env` offline. Test a restore once: `gunzip -c file.sql.gz | docker compose exec -T db psql -U owner wifibms` into a scratch database.

## Limits to know
- **Internet down:** customers cannot reach the portal or pay (it is reached through Cloudflare). Customers already connected keep browsing as long as Starlink is up; local logins that need no payment still work because the site agent talks to the API locally.
- **Machine or power failure** takes the platform down. Keep the UPS, backups and a spare disk image. A VPS later is easy: copy a database dump and use the normal `docker-compose.yml`.
- The admin pages are public on the internet behind sign-in, roles and 2-step verification. For extra safety add a Cloudflare **Access** policy on `wifi.waste2light.com/admin*` limiting it to your email.
- `deploy/Caddyfile.tunnel` and `docker-compose.tunnel.yml` are written to documented behaviour; check the first start with `docker compose logs caddy cloudflared`.
