# API reference

All paths are under `https://<host>`. JSON in, JSON out. Errors: `{"error": "message"}` with 400 / 401 / 403 / 404 / 429 / 502 / 500.
Bearer tokens go in `Authorization: Bearer <token>`. `:tenant` is the operator's slug.

## Public (portal)
| Method | Path | Purpose |
|---|---|---|
| GET | `/healthz` | liveness |
| GET | `/v1/t/:tenant/sites/:site/plans` | plans for sale |
| POST | `/v1/t/:tenant/orders` | `{site, plan_id, phone, email?}` → Paystack `authorization_url` |
| GET | `/v1/t/:tenant/orders/:reference` | poll status; shows the access code once provisioned |
| POST | `/v1/t/:tenant/webhooks/paystack` | Paystack only (signature checked) |
| POST | `/v1/t/:tenant/connect` | `{site, mac, code, accept_terms}` → one-time ticket |
| POST | `/v1/t/:tenant/complaints` | report a problem `{phone, message, category?, order_ref?}` (rate limited) |
| GET | `/v1/t/:tenant/brand` | name, tagline, support phone, colour, router hosts, sites: what the web app shows |
| POST | `/v1/t/:tenant/client-errors` | the web app reports browser errors (rate limited) |
| GET/POST | `/v1/t/:tenant/orders/:reference/refund` | customer refund request `{reason?}` (the unguessable reference is the proof; 14-day window; rate limited) / its status |

## Members (`mt_` token from register/login)
`POST /members/register|login|logout|otp/request|otp/verify|claim|connect|wallet/topup|wallet/buy|devices/swap`,
`GET /members/me|wallet|devices` (all under `/v1/t/:tenant`).

## Staff (`at_` token)

Roles, from most to least access: **system_admin** (everything, including site keys), **ceo** (everything except site keys; manages
admins and customer reps), **admin** (day-to-day: plans, orders, refund requests, special vouchers, resolving errors),
**customer_rep** (customers, manual sales, complaints, error log). The server checks a permission on every call
(`GET /admin/me` lists yours); the table below shows the lowest role that may call each endpoint.

Sign-in is two steps: `POST /admin/login {email, password}` returns `{challenge, method, sent_to}`, then
`POST /admin/login/verify {challenge, code}` returns the token. An account with no 2-step method yet gets a limited token and
`setup_required: true`; until it enrols (`POST /admin/2fa/start {method: totp|email|sms, phone?}`, `POST /admin/2fa/confirm
{challenge, code}`) every other call answers `403` with `code: "mfa_required"`.

| Method | Path | Lowest role |
|---|---|---|
| POST | `/admin/login`, `/admin/login/verify` | public |
| POST/GET/PUT | `/admin/logout`, `/admin/me`, `/admin/2fa/start`, `/admin/2fa/confirm`, `/admin/password` (`{current, new}`) | any staff |
| GET | `/admin/customers?q=&limit=&offset=`, `/admin/customers/:id` | customer_rep |
| GET | `/admin/plans` | customer_rep |
| POST/PUT | `/admin/plans`, `/admin/plans/:id`, `/admin/plans/:id/active` | admin |
| GET | `/admin/overview?range=24h\|7d\|30d\|90d`, `/admin/analytics/{revenue\|traffic\|devices\|plans}?range=`, `/admin/network`, `/admin/security` | admin |
| GET | `/admin/orders?q=&status=`, `/admin/refunds` · POST `/admin/refunds/:id/decline` | admin |
| POST | `/admin/orders/:id/refund` | ceo. One transaction under the order lock: Paystack refund (rolls back if Paystack refuses), or wallet credit; cash and voucher orders are refunded by hand |
| POST | `/admin/vouchers/manual` `{plan_id, site, account_id?\|phone?, method: cash\|transfer}` (price always comes from the plan) | customer_rep |
| GET | `/admin/vouchers?kind=manual\|special&q=` | customer_rep (special ones only for admin and above) |
| POST | `/admin/vouchers/special` `{label, site, devices, count (1-50), valid_for?, down_kbps?, up_kbps?}`, `/admin/vouchers/:id/revoke` | admin |
| GET | `/admin/complaints?status=&q=`, `/admin/complaints/:id` · PUT same `{status?, assign?: "me"\|"none"}` · POST `/admin/complaints/:id/notes` `{body}` | customer_rep |
| GET | `/admin/errors?state=&source=&severity=&site=&q=&before=&limit=`, `/admin/errors.csv`, `/admin/errors/summary`, `/admin/errors/:id` | customer_rep |
| POST | `/admin/errors/:id/resolve` `{all_same?}` | admin |
| GET/POST | `/admin/users?status=all`, `/admin/users` | ceo (system_admin may also manage ceo and system_admin accounts) |
| PUT/DELETE | `/admin/users/:id` (role, name, phone, status; delete = remove, history kept) · POST `/admin/users/:id/reset-password`, `/reset-2fa` · GET `/admin/users/:id/activity` | ceo |
| GET | `/admin/audit?action=&actor=&before=&limit=`, `/admin/audit/verify`, `/admin/audit.csv?from=` | ceo |
| PUT | `/admin/brand` `{brand?, tagline?, supportPhone?, primary?, hotspotHosts?}`, `/admin/secrets/paystack` | ceo |
| POST | `/admin/node-tokens`, `/admin/node-tokens/:id/revoke` | system_admin |

### Plans
`POST /admin/plans` takes `kind` (`time`, `data` or `hybrid`), `price_kobo`, and:
`duration {unit: hour|day|week|month, count}` (connected time), `data_bytes`, `valid_for {unit, count}` (expiry after first use),
`device_limit`, and speed as `down_kbps`/`up_kbps` or `speed_profile_id`. Leave speed out for **2 Mbps up and down**.
Seconds can be given directly with `active_seconds` / `valid_seconds`.

## Site agent (`nt_` token)
`POST /v1/node/radius/authorize`, `POST /v1/node/radius/accounting`, `POST /v1/node/metrics`, `POST /v1/node/errors` (up to 50 local problems per call, shown in the error log).
