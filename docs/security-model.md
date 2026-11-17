# Security and privacy model

## Trust boundaries
| Boundary | Credential | Notes |
|---|---|---|
| Customer → API | access code / `mt_` token | codes are 8 chars from a 32-letter alphabet; guessing is rate limited per IP and per code |
| Admin → API | password + TOTP → `at_` token (12 h) | 5 failures lock 15 min; roles viewer < operator < owner |
| Site agent → API | `nt_` token (one per site, revocable) | the only credential that crosses the internet from a site |
| FreeRADIUS → agent | `X-Agent-Key` on loopback | agent refuses non-loopback `LISTEN` |
| Paystack → API | HMAC-SHA512 signature | plus a server-side Verify Transaction call |
| API → PostgreSQL | `platform_login` ∈ `platform_app` | not superuser, not table owner → RLS applies |

## Why tickets
The hotspot login goes through RADIUS in clear-ish form. Instead of passing a code or password, the portal first
verifies the customer, then mints a **random one-time ticket** (username `tk…`, password `tp_…`, 120 s, bound to the MAC).
Nothing long-lived travels over RADIUS.

## Data we keep (for audit and billing)
Phone, device MAC/OS/hostname/class, IP, session start/end, bytes, consent record (terms version, time, IP, user agent),
orders, payments, wallet ledger, admin actions. Raw card data never touches us (Paystack hosts checkout).

## Compliance (Nigeria): to confirm with counsel
- **NDPA 2023:** lawful basis and notice, retention period, data-subject access/erasure, breach notification (72 h), DPO/registration thresholds.
- **NCC:** any lawful-intercept or traffic-log retention duty for ISPs.
- Decide a retention period for `sessions`, `audit_log` and `consents`, and add a scheduled job; the schema keeps them indefinitely today.
- The consent text (`TERMS_VERSION` in `api/src/radius.rs`) is a placeholder version label. The terms themselves are yours to write.

## Known limits
Tether detection is heuristic; MAC randomisation; in-memory rate limiter; TOTP not single-use.
