# Security (ROADMAP Phase 25)

## Threat model (personal single-account deployment)

| Asset | Threat | Mitigation |
|---|---|---|
| Shopee session cookies | leakage via logs/traces/notifications | `SecretString` redaction; `CookieStore` never logs values; centralized `redact` helpers; fixtures greped for `SPC_*`/`set-cookie` |
| Browser profile | theft from disk | `0700` dir, service-user owned, git-ignored, off Docker images |
| Telegram bot token | leakage | redaction; `TelegramNotifier` sanitizes errors (strips URL + literal token) |
| Database credentials | leakage / public exposure | env/secret file only; no public DB port in compose |
| Admin endpoints | unauthorized control | localhost/private bind; mutating ops require `x-admin-token` |
| Browser DevTools/CDP | remote takeover | bind localhost only; never exposed |
| Upstream compromise | malformed responses driving bad actions | schema-tolerant parsing; unknown responses classified + bounded; no blind retries |

Out of scope by charter: CAPTCHA/verification bypass, fingerprint spoofing,
proxy rotation, multi-account abuse, checkout/payment automation.

## Secret inventory

| Secret | Where it lives | Protection |
|---|---|---|
| Shopee cookies | `SESSION_COOKIE_STORE_PATH` (`0600`), browser profile | never committed, redacted in logs |
| Telegram bot token | `TELEGRAM_BOT_TOKEN` (env / `.env`) | `.env` git-ignored; redacted |
| Database password | `DATABASE_URL` / `POSTGRES_PASSWORD` | env / secret file; no public port |
| Admin token | `ADMIN_TOKEN` (env) | required for mutating admin ops |
| External feed auth (if any) | `EXTERNAL_FEED_URL` / source config | treated as secret |

## Hardening posture

- Runtime container runs as a non-root `hunter` user (see `docker/Dockerfile`).
- Only the private admin/health port is opened; it is never published.
- `.gitignore` excludes `.env*`, cookies, browser profiles, DBs, logs.
- Redaction is centralized so new call sites inherit it; a redaction audit test
  (`observability`) asserts every documented secret-bearing key name is masked.
- Dependency/security policy enforced by `cargo deny` (rustls only, single async
  runtime, license allowlist) in CI, plus secret scanning.

## Before merging session-touching code

Confirm: no secret written to logs; session files git-ignored; Docker volume
permissions documented; diagnostics redact sensitive headers; admin endpoints
authenticated; browser debug bound to localhost; backups do not include
unencrypted session secrets unless explicitly intended.
