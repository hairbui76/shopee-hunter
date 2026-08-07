# Research workspace

This directory holds **reference material only**. Nothing in `research/` may be
imported, linked, or copied verbatim into production crates.

## Reference repositories

Clone reference repositories under `research/reference-repos/` (git-ignored
content is fine; only notes are committed):

```text
research/reference-repos/
├── Bot_Voucher/          # https://github.com/trongthaohub/Bot_Voucher
├── shopee-voucher-tool/  # https://github.com/vinh781/shopee-voucher-tool
└── shop-watcher/         # https://github.com/NgVB1408/shop-watcher
```

For each repository record in `research/notes/<repo>.md`:

- repository URL and the exact commit hash inspected;
- useful files/functions and what they demonstrate;
- observed assumptions (endpoints, schemas, timing);
- anything obviously stale or unsafe;
- endpoint names seen;
- fields needed by later phases.

## Rules

1. Never commit cookies, tokens, session dumps, or unsanitized captures.
   Raw captures belong in `research/**/captures/` which is git-ignored.
2. Sanitize every fixture before moving it into `tests/fixtures/`.
3. Treat every private Shopee endpoint observed here as **unstable**; production
   code must wrap it behind an adapter with fixture tests.
4. Production crates must never depend on files in this directory.
