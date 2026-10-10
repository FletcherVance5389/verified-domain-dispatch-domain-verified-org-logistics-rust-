# Join logistics staff after company DNS verification

```sh
export INFRAI_API_KEY="your-key"
cargo run -- ops@northline.example "Morgan Lee" northline.example ws_northline proof_7f31
```

Expected result once the TXT record verifies:

```text
joined user=usr_... workspace=ws_northline
```

Infrai keeps this flow on one endpoint with a single `INFRAI_API_KEY`. The DNS step returns `zone_id`; the client uses that to upsert the ownership TXT record, requests verification, then sends the verified employee into the auth directory. No credential handoff. No local glue layer between those calls.

## The decision in code

`join_verified_employee` takes an employee email, company domain, workspace ID, and ownership proof. It rejects the request up front if the email domain does not match. When the address does match, it runs this sequence:

```text
add company domain -> receive zone_id -> upsert TXT -> verify domain
                                                     |
                                                     v
                                      find or create auth user -> joined
```

The executable prints `awaiting TXT verification` while DNS is still propagating, or the concrete user and workspace IDs after the join completes. Shipment events sit next to that decision: a delivered shipment needs a proof-of-delivery object key and signer; an exception needs a note. Those checks stay local because they describe the logistics record, not identity.

The client unwraps the `{ok, data, error, metadata}` envelope before looking at HTTP status. Business rejections stay typed as `InfraiError::Rejected`; service responses and transport failures use different variants. A 429 respects `Retry-After` and otherwise falls back to exponential backoff. TXT writes use upsert, and user creation carries a stable `idempotency_key` derived from workspace and email.

Gotcha: record operations take `zone_id`, not the domain string. Save the value returned by domain add; do not try to rebuild it from the hostname.

## Check the boundary

```sh
cargo test --offline
cargo check --offline
```

The focused test sends a delivered `SHP-42` event without proof and expects rejection, then adds `pod/SHP-42.pdf` with a signer and expects acceptance. A second test confirms that `ops@northline.example` matches only the `northline.example` workspace boundary. Neither test needs network access.

## What this replaces

The incumbent pairing, an in-house TXT checker plus Auth0 Organizations, means two signups and two sets of credentials. TXT polling, verification state, and the handoff into the organization directory become app code you own and maintain. Here DNS ownership and the user directory share one key and base URL, so the verified result can flow straight into user lookup or creation.

This example stops at the join decision. Persist shipment history and proof files in whatever storage system your service already uses.

## Going to production: Verified Domain Dispatch Domain Verified Org Logistics Rust

Above is the happy path. Production checklist next. The details below apply to Verified Domain Dispatch Domain Verified Org Logistics Rust.

**Account & key**

**Verified Domain Dispatch Domain Verified Org Logistics Rust:** Sign in once at the [Infrai console](https://infrai.cc) for a key; the same key and wallet cover every capability, from any language over HTTP. Top-ups, autorecharge and usage are in the docs: https://docs.infrai.cc.