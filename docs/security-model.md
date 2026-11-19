# Security and privacy model

## Trust boundaries
| Boundary | Credential | Notes |
|---|---|---|
| Customer → API | access code / `mt_` token | codes are 8 chars from a 32-letter alphabet; guessing is rate limited per IP and per code |
| Staff → API | password, then a 6-digit code (authenticator app, email or SMS) → `at_` token (12 h) | 5 wrong passwords or codes lock 15 min; codes last 10 min, are stored hashed and die after 5 tries; roles system_admin, ceo, admin, customer_rep (one permission table in `api/src/admin.rs`) |
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

## Staff roles and accountability
- Every action by staff is written to the hash-chained audit log with who did it; system admins and the CEO can filter it by person.
- Managers can only manage roles below them; the last active system admin can never be removed, demoted or suspended.
- Special (unlimited) vouchers need a reason, are limited to admin and above, and every code records who made it. Customer reps cannot see them.
- Removing a staff account signs them out at once and keeps their history.

## Known limits
Tether detection is heuristic; MAC randomisation; in-memory rate limiter; TOTP not single-use.

## Device monitoring
The site agent only reads the hardware (ping, SNMP GET, RouterOS read-only user, Omada read-only app) and sends the result over the same `nt_` node channel. `devices.json` holds those credentials: keep it `chmod 600`, owned by the agent user, and use a dedicated read-only account on each device. Reported values are clamped, MACs validated and device/client counts capped on arrival; device rows are tenant-isolated by row-level security like everything else, and customer names shown beside a client are visible to the same staff roles that can already see customers.
