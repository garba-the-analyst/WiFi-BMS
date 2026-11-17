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

## Members (`mt_` token from register/login)
`POST /members/register|login|logout|otp/request|otp/verify|claim|connect|wallet/topup|wallet/buy|devices/swap`,
`GET /members/me|wallet|devices` (all under `/v1/t/:tenant`).

## Admin (`at_` token; viewer / operator / owner)
| Method | Path | Role |
|---|---|---|
| POST | `/admin/login` `{email, password, totp?}`, `/admin/logout`, `/admin/totp/setup`, `/admin/totp/confirm` | any |
| GET | `/admin/overview?range=24h\|7d\|30d\|90d` | viewer |
| GET | `/admin/analytics/{revenue\|traffic\|devices\|plans}?range=` | viewer |
| GET | `/admin/customers?q=&limit=&offset=`, `/admin/customers/:id` | viewer |
| GET | `/admin/plans`, `/admin/speed-profiles`, `/admin/network`, `/admin/security` | viewer |
| GET | `/admin/audit?action=&before=&limit=`, `/admin/audit/verify`, `/admin/audit.csv?from=` | viewer |
| POST/PUT | `/admin/plans`, `/admin/plans/:id`, `/admin/plans/:id/active`, `/admin/speed-profiles` | operator |
| POST | `/admin/node-tokens`, `/admin/node-tokens/:id/revoke` | owner |
| PUT | `/admin/secrets/paystack` | owner |

## Site agent (`nt_` token)
`POST /v1/node/radius/authorize`, `POST /v1/node/radius/accounting`, `POST /v1/node/metrics`.
