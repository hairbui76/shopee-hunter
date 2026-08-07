# Research notes

Phase 5 of the roadmap systematizes what the later integrations need to know.
The three reference repositories named in CLAUDE.md are **not vendored** into
this repo (they carry stale schemas, DB files, and credentials). Clone them
under `research/reference-repos/` locally for study; record the commit hash you
inspected in the per-repo note before drawing on it.

```text
research/reference-repos/
├── Bot_Voucher/          # https://github.com/trongthaohub/Bot_Voucher
├── shopee-voucher-tool/  # https://github.com/vinh781/shopee-voucher-tool
└── shop-watcher/         # https://github.com/NgVB1408/shop-watcher
```

## What has already been captured

The unstable Shopee behavior needed by the claim path is encoded — with
sanitized fixtures — in the `shopee-hunter-client` crate rather than left in
prose:

- endpoint registry and request shapes: `crates/shopee-client/src/endpoints.rs`
  and `plan.rs` (every path marked `UNSTABLE`, dated);
- response envelopes and error codes: `crates/shopee-client/src/dto.rs`;
- response classification (12 result classes, 6 session-probe states):
  `crates/shopee-client/src/classify.rs`;
- 25 sanitized fixtures: `tests/fixtures/shopee/` (`origin: synthetic`,
  `capture_date: null` until replaced by a live capture).

See [field-inventory.md](field-inventory.md) for the observed field table.

## Discipline

- No secrets in fixtures. Fixtures are greped for `SPC_*`, `set-cookie`,
  `bearer`, etc. by a `shopee-client` test.
- Every private endpoint is an assumption behind an adapter with a fixture test.
- Synthetic fixtures must be replaced by redacted live captures before their
  `capture_date` is set; a metadata test enforces this once `origin` changes.
