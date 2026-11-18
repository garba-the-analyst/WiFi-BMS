# WiFi-BMS

A multi-tenant **Wi-Fi subscription platform** for neighbourhood ISPs: captive portal,
Paystack payments, member accounts and wallet, time/data/hybrid plans that count
**active connected time only**, hotspot-sharing (tethering) detection, a back office with live
analytics, and a tamper-evident audit trail. Built first for a Starlink business in Minna, Nigeria,
and designed so another operator can be onboarded as a new *tenant*.

```
 customer phone ─▶ router hotspot ─▶ FreeRADIUS ─▶ site-agent (per site) ──HTTPS──▶ central API ─▶ PostgreSQL
        │                                  ▲            │  CoA / Disconnect             ▲            (row-level security)
        └── portal (pay / sign in) ────────┼────────────┘                               │
                                           └──── Paystack webhook ──────────────────────┘
```

| Part | Path | What it is |
|---|---|---|
| Entitlement engine | `entitlement/` | Pure Rust: plans, active-minute metering, fair use, device slots, RADIUS limits. No I/O. |
| Central API | `api/` | Axum + sqlx. Money path, RADIUS link, members, wallet, admin, hardening. |
| Site agent | `site-agent/` | Runs at each site. Bridges FreeRADIUS to the API, queues usage offline, pushes CoA/Disconnect. |
| Schema | `migrations/` | PostgreSQL 16 with row-level security, append-only ledgers, hash-chained audit log. |
| UI designs | `ui/` | Portal and admin boards (design source, not yet wired to the API). |
| Web app | `web/` | Vue 3 single-page app: customer portal at `/`, staff back office at `/admin`. Caddy serves it and proxies `/v1/*`. |
| Deployment | `Dockerfile`, `docker-compose.yml`, `deploy/` | API + DB + Caddy; site-agent unit, FreeRADIUS and RouterOS configs. |
| Docs | `docs/` | `api.md`, `deployment.md`, `site-setup.md`, `security-model.md` |

## How a customer's session works

1. Customer buys a plan on Paystack (guest gets an **access code**; a member can also pay from a **wallet**).
2. Paystack calls our webhook. We check the HMAC signature, **re-verify the transaction with Paystack**,
   compare amount and currency with our own order, then grant the entitlement exactly once.
3. Customer enters the code (or signs in). The portal issues a **one-time ticket** bound to the device MAC (120 s).
4. The hotspot sends the ticket through RADIUS to the site agent, which asks the central API. The API picks the
   purchase expiring soonest, checks device slots, and answers with `Session-Timeout`, speed and data limits.
5. Every minute the router reports usage. A minute counts as **active** only if at least 20 KB moved; idle
   devices are disconnected after 5 minutes. The agent moves the router's time limit out by the idle time, so
   idle minutes do not cost plan time.
6. Tether signals are scored; a single signal never disconnects (warn 30+, throttle 60+, disconnect 75+).

## Run the tests

```bash
cargo test --workspace --lib --bins             # engine, API units, site agent (no database)
cargo test -p platform-api --test hardening     # headers, CORS, body limits
# Integration tests need a PostgreSQL 14+ where you may create databases and roles:
PGHOST=localhost PGUSER=postgres PGPASSWORD=... bash api/run_integration.sh
```

`run_integration.sh` builds a throwaway database, applies every migration, creates a **non-superuser** login so
row-level security is really exercised, runs the suites (`money_path`, `radius_link`, `members`, `admin_api`) and drops it.
CI does all of this on every push (`.github/workflows/ci.yml`).

## Run it

See `docs/deployment.md` (central) and `docs/site-setup.md` (each site). In short:

```bash
cp .env.example .env              # fill in values; PLATFORM_MASTER_KEY = openssl rand -hex 32
docker compose up -d db
docker compose --profile tools run --rm migrate
docker compose up -d --build api caddy   # edit deploy/config.json first (tenant, site, brand)
ADMIN_PASSWORD='...' docker compose run --rm api create-admin <tenant-slug> you@example.com owner
```

## Honest status

Done and tested against a real PostgreSQL: money path, RADIUS link, member sign-in and wallet, admin API, refunds, hardening. The web app was driven in a real browser against the live API (portal, member sign-up, wallet purchase, refund request, owner refund, every admin page). Not yet verified: Caddy/Cloudflare on the real VPS, and real router hardware.

**Not done yet / needs your attention before launch**
- **Real hardware validation.** The FreeRADIUS `rlm_rest` JSON shape, `radclient` CoA/Disconnect and the RouterOS
  REST lookups are written to documented behaviour but have **not** been run against a real MikroTik. Do this on the bench first.
- **SMS provider.** `DisabledSms` is wired in production. Implement `SmsSender` for Termii/Africa's Talking/Twilio
  (one small function) to enable SMS sign-in and sending access codes.
- **Frontend.** `ui/` holds designs; the portal and admin apps are not built against the API yet.
- **Tether observations.** The agent does not yet measure TTL/connection counts; today detection relies on device
  slots and mixed device fingerprints. TTL can also be spoofed by a determined user: tether detection deters
  casual sharing, it does not make it impossible.
- **Per-session admin disconnect** and **customer refunds** are not exposed in the admin API yet.
- **Legal.** Confirm data retention, consent text and the privacy notice with a Nigerian lawyer (NDPA 2023, NCC rules).
  See `docs/security-model.md`.
