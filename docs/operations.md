# Mature operations (ROADMAP Phase 36)

The system integrates unstable external behavior, so there is no final "done"
state — the mature state is reproducible deployment, documented recovery,
measurable sources, bounded/auditable claims, owner alerting, isolated upstream
changes, tested backups, and routine (not emergency) maintenance.

## Recurring checks

### Weekly

- Review unknown Shopee responses (they are stored redacted and alerted).
- Inspect per-source health and last successful collection.
- Check session failures / manual-intervention frequency.
- Review rate-limit incidents and scheduler lag (precision reports).

### Monthly

- Update dependencies; run `cargo deny check` / audit.
- Validate backups with a restore exercise (docs/disaster-recovery.md).
- Inspect DB growth; confirm retention (Phase 33) is keeping it bounded.
- Remove or degrade consistently poor sources (Phase 27 analytics).
- Refresh fixtures when upstream behavior changed.

## Performance review (trends, not anecdotes)

- Which source finds useful vouchers first? (`first_discovery_wins`, lead time)
- How often are discovered vouchers actually claim-ready?
- What fraction of claim attempts succeed; which failure classes dominate?
- Is polling volume justified by unique discoveries? (requests-per-discovery)
- Is scheduler/network latency materially affecting outcomes? (precision reports)
- How often does session maintenance need manual action?

## Architecture-change triggers (evidence required)

Only revisit the modular monolith when data shows the need:

- split the browser/session process if Chromium destabilizes the core;
- add Redis only if DB-backed coordination is a measured bottleneck;
- separate collectors if one source needs incompatible runtime deps;
- move the notifier to its own worker if delivery backlog becomes significant.

## Operator surfaces

- Health/metrics/admin API (`/health/*`, `/metrics`, `/admin/*`) — private bind.
- Telegram admin commands (`/status`, `/session`, `/jobs`, `/recent`,
  `/pause_claims`, `/resume_claims`, …) — owner-allowlisted.
- Alerts delivered to the owner for the failure conditions in
  docs/observability.md.
