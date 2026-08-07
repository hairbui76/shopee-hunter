# Voucher request/response field inventory

Observed fields relevant to discovery, identity, and save/claim. Not all fields
are available from every source; the canonical model treats most as optional.

| Field | Meaning | Used for | Availability |
|---|---|---|---|
| `voucher code` | Human voucher code | display, fallback identity, fallback claim | often |
| `promotion_id` | Shopee promotion identifier | identity (priority 2), claim body | Shopee sources |
| `signature` | Save/claim signature token | claim body | Shopee sources only |
| `start_time` / `end_time` | Activation window (epoch or ISO) | scheduling, validity | usually |
| `min_spend` | Minimum basket to use | ranking, combo analysis | usually |
| `discount_amount` | Fixed discount value | ranking, combo analysis | fixed-amount vouchers |
| `discount_percent` | Percentage discount | ranking | percentage vouchers |
| `max_discount` / cap | Cap on percentage discount | ranking, combo analysis | percentage vouchers |
| `scope` | platform / shop / category / payment | identity, eligibility | usually |
| `payment_method` | Restricted payment method | eligibility | payment vouchers |
| `usage / status indicators` | Remaining quantity, claimed flag | classification | sometimes |
| stable external ID | Source-provided unique id | identity (priority 1) | external feeds |

## Identity priority (implemented in `domain::identity`)

1. source-provided stable external ID (source-scoped);
2. trusted `promotion_id` (global);
3. canonical composite fingerprint of
   `(source, normalized code, start, end, scope, type, spend, discounts)`.

## Save/claim body (highest-risk assumption)

`crates/shopee-client/src/plan.rs` builds the save request from
`voucher_promotionid` + `signature`, falling back to `voucher_code`. This is
untested against a live endpoint and must be verified by an opt-in live smoke
run before auto-claim is enabled in production.
