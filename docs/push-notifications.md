# Push notifications

FCM display text is localized in French, English, Spanish and Italian. Match alerts use the
partner's current first name, resolved from the authorized match at delivery. Names are bounded
and control/bidirectional characters removed. Message contents, photos, request replies and
administrator notes never appear in push data or copy. Devices retain the existing opt-out rules.

| Type | Trigger | Mobile destination |
| --- | --- | --- |
| `new_match` | Reciprocal like creates a match | Conversation |
| `new_message` | A new, still unread incoming message | Conversation |
| `match_expiring` | Confirmed chat has seven inactive days left | Conversation |
| `continuation_requested` | Partner chooses to continue; recipient has not chosen | Conversation |
| `conversation_continued` | Both choices are confirmed | Conversation |
| `photos_revealed` | Photo sharing changes to mutual | Conversation/photo choices |
| `first_period_ending` | One reminder within two hours of the first 24-hour deadline | Conversation |
| `data_request_replied` | A public reply accompanies a request status update | My requests, matching entry expanded |
| `new_session` | A new refresh-token family opens while another push device exists | My sessions |
| `billing_payment_failed` | An outstanding Stripe invoice payment fails | Subscription |

There is no advertising-credit notification. Before the first period ends, continuation is not
yet available: its reminder explains that the choice comes afterwards.

## Delivery and isolation

Business writes enqueue on the same PostgreSQL transaction. Replayed choices do not notify again.
Match maintenance schedules the bounded reminder batches; `reminded` contributes to workload counts.
The outbox rechecks access, blocks, deadlines, choices, grant version and request/session state.
Withdrawn/replaced photo grants and superseded choices cannot revive queued alerts. Existing message
read and failed-payment checks remain. New lifecycle/security alerts, including request replies,
are retained for at most one day; the first-period reminder expires at its match deadline.
FCM/APNs storage is capped at one day and at the earliest notification, match or recipient-session
deadline. New-session alerts also respect the opened session's deadline, using
[Android TTL and APNs expiration](https://firebase.google.com/docs/cloud-messaging/customize-messages/setting-message-lifespan).
Alerts already accepted by the provider cannot be recalled by these database checks.

New-session alerts exclude the new family, both when scheduling and sending. Refresh rotation does
not create them. Session/account revocation and device removal invalidate pending deliveries.
Names stay outside routing metadata. The client accepts only known kinds and UUID identifiers,
resynchronizes authentication, then rereads the authorized destination. Foreground stays silent.

## Rollout

No new environment variables. Apply the migration with the migration role, then run matching API,
outbox and maintenance versions. Stop the old worker before the new API produces these event kinds.
The `notification events` section of `db/001_schema_postgres.sql` defines the type constraint and
photo-grant generation once. `db-migrate` reuses that section for existing databases; a fresh
installation applies it as part of the complete schema.
Retired trial notifications are removed before restoring the current type constraint; other
notifications and their deadlines are preserved. Startup rejects a schema that has not been upgraded.

```bash
cargo run --locked --bin db-migrate
```

## Organization and verification

`notifications/domain.rs` owns notification kinds and retention durations. `enqueue.rs` persists
notifications and delivery jobs in the business transaction. `delivery/pg.rs` resolves current
eligibility, names and the earliest delivery deadline; only its SQL text is cached, never access
decisions. `delivery.rs` coordinates delivery and `push.rs` owns the FCM transport.

From the API root:

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets
```

For integration checks, prefer the disposable PostgreSQL launcher. It uses local PostgreSQL/PostGIS,
generated credentials and loopback, verifies repeated upgrades, then removes its container and
fixtures. It requires Docker access and the local PostgreSQL image described in [Tests](../tests/README.md).

```bash
cargo build --locked --bin db-migrate
python3 scripts/verify-continuation-postgres.py \
  --native-migrator target/debug/db-migrate \
  --test notifications_postgres --test matches_postgres --test matches_inactivity_postgres \
  --test privacy_rights_postgres --test mobile_identity --test maintenance_outbox_postgres \
  --test account_erasure_postgres --test localization_postgres
```

The tests cover delivery deadlines, reply retention, changing names, old photo grants, continuation
replays and recipients, reminder deduplication, request replies without private text and new-session
exclusion. The upgrade test preserves existing notifications, match deadlines and photo-grant
generations. Never point these fixture tests at production.

Local validation on **5 November 2026**: **380 Rust tests passed** with
`cargo test --locked --all-targets` and **39 PostgreSQL tests passed** across the eight suites above,
including repeated schema upgrades. Formatting and Clippy with all targets/features and warnings
denied passed. The disposable database, fixtures and generated credentials were removed after
the run. Provider calls were disabled; this does not validate device reception.

On devices, verify background/cold-start delivery, four languages, the actual current first name,
mutual choices, withdrawn grants, resolved alerts, first-period timing, new-session exclusion and
both privacy destinations. A successful FCM request does not prove OS presentation.
