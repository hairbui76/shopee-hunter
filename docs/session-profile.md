# Session profile & cookie storage

Shopee authenticated state is sensitive. This document is the reference for
where session material lives and how it is protected (ROADMAP Phases 9–10,
CLAUDE.md "Session handling rules", ARCHITECTURE.md §12/§21).

## Locations

| Purpose | Default path | Owner | Mode |
|---|---|---|---|
| Persistent browser profile | `/var/lib/shopee-hunter/browser-profile/` | service user | `0700` dir |
| HTTP cookie material | `/var/lib/shopee-hunter/session/cookies.json` | service user | `0600` file |

Both paths are configurable (`SHOPEE_PROFILE_PATH`, `SESSION_COOKIE_STORE_PATH`)
and are git-ignored (`browser-profile/`, `session-state/`, `*.cookies*`).

## Rules

- Never commit cookies or browser profiles.
- Never print full `Cookie` headers; the `CookieStore` never includes cookie
  values in error messages, and `shopee-client`'s `SecretString` redacts on
  `Debug`/`Display`.
- The cookie file is written with `0600` permissions by `CookieStore::save`.
- The browser profile directory should be `0700` and owned by the service user.
- Browser DevTools / CDP endpoints must bind to localhost only and never be
  exposed publicly.

## Bootstrap workflow

1. On the host, run (with a local Chromium available):

   ```bash
   SHOPEE_PROFILE_PATH=/var/lib/shopee-hunter/browser-profile \
     cargo run -p shopee-hunter-tools --bin login_session --features browser
   ```

2. Log in manually in the opened browser window (including any verification
   challenge — the tool never bypasses one), then press Enter in the terminal.
3. The profile persists across restarts. The service reads session state from
   it / from the cookie store; it never re-derives credentials from source.

## Session states

The session manager tracks: `UNKNOWN`, `HEALTHY`, `DEGRADED`, `EXPIRED`,
`LOGIN_REQUIRED`, `VERIFICATION_REQUIRED`, `DISABLED`. The claim worker is
paused (claim gate closed) unless the state is positively `HEALTHY`.
