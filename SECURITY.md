# Security policy

## Reporting a vulnerability
Email the maintainer privately (see the repository owner's profile). Please do not open a public issue for
security problems. Include steps to reproduce; expect an acknowledgement within a few days.

## What is protected, and how
See `docs/security-model.md` for the full model. Highlights:

- **Money:** Paystack webhooks are authenticated (HMAC-SHA512, constant-time), re-verified with Paystack's API,
  amount/currency-checked against our own order, idempotent by order reference, and backstopped by a UNIQUE constraint.
- **Tenants:** every request runs in a transaction with `app.tenant_id` set; PostgreSQL row-level security enforces it.
  The API connects as a non-superuser, non-owner role.
- **Secrets:** Paystack keys are AES-256-GCM encrypted with `PLATFORM_MASTER_KEY` (environment only). Passwords are Argon2id.
  Bearer tokens (`mt_`, `at_`, `nt_`) are random, shown once, stored as SHA-256.
- **Audit:** append-only, hash-chained; `/admin/audit/verify` reports any altered row.
- **Abuse:** per-IP and per-identity rate limits, account lockout, 64 KB body limit, 30 s request timeout, strict CORS.

## Operator checklist
1. Keep `PLATFORM_MASTER_KEY` backed up offline; losing it makes stored Paystack keys unreadable. Rotating it needs a re-encrypt.
2. Every staff account must use 2-step verification (the app enforces it). Keep the number of system admins small.
3. Set `TRUST_PROXY=1` **only** behind your own reverse proxy (the compose file does this); otherwise client IPs cannot be trusted.
4. Use `sk_live_` keys only after a full test-mode run. Restrict the Paystack dashboard IP allow-list/webhook URL.
5. Never expose PostgreSQL or the site agent's port (it binds to loopback by design).
6. The site agent's RouterOS REST credentials should belong to a read-only router user.

## Known limits
- TTL-based tether detection can be spoofed; MAC randomisation weakens device identity (iOS/Android private addresses
  change per network, so the "device" is best-effort).
- Rate limiting is in-memory per API process; run one API instance, or move limits to the proxy, before scaling out.
- TOTP codes are not yet single-use within their 30 s window.
