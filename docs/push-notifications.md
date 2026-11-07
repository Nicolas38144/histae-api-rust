# Push notifications

FCM display text is localized in French, English, Spanish and Italian. Match alerts use the
partner's current first name, resolved from the authorized match at delivery. Names are bounded
and control/bidirectional characters removed. Message contents, photos, request replies and
administrator notes never appear in push data or copy. Devices retain the existing opt-out rules.

## User choices

`GET/PATCH /api/users/me/notification-preferences` reads or changes account-wide booleans:
`messages`, `new_matches`, `continuations`, `photos`, `reminders`, `account_security` and `discreet`.
Categories default to enabled and discreet display defaults to disabled. PATCH updates only supplied
fields, so one device does not overwrite another device's unrelated choices. Device permission,
registration and existing opt-outs remain independent; changing a category never registers a device.
The account is locked and its current active status rechecked before a write, preventing a request
started before account erasure from recreating deleted preferences after erasure commits.

| Category | Types / Android channel |
| --- | --- |
| `messages` | `new_message` / `histae_messages` |
| `new_matches` | `new_match` / `histae_matches` |
| `continuations` | `continuation_requested`, `conversation_continued` / `histae_continuations` |
| `photos` | `photos_revealed` / `histae_photos` |
| `reminders` | `first_period_ending`, `match_expiring` / `histae_reminders` |
| `account_security` | `new_session`, `data_request_replied`, `billing_payment_failed` / `histae_account` |

`GET/PUT /api/matches/{id}/notification-preferences` reads or replaces a private participant mute.
PUT accepts `muted` and `muted_until`: false/null unmutes, true/null mutes indefinitely, and true
with a future timestamp mutes until that instant (at most 31 days). The app offers one hour,
tomorrow at local midnight, and indefinite mute. Only the requesting participant can read or change
their choice on a currently authorized match; the partner is never notified. Expired mutes read as
inactive. A conversation mute suppresses all its match alerts on all devices, regardless of category.

Enqueue and delivery share the same preference predicate. Disabled categories and active mutes do
not create push jobs, preventing replay of muted-period events. Existing jobs recheck current
choices before sending. Inbox events, SSE, unread counters and messages remain independent.
Discreet mode omits the partner's first name at delivery and uses the existing four-language generic
copy; routing data never gains display names or message text. Already accepted provider alerts
cannot be recalled. There is no scheduled quiet-hours feature.

Both tables cascade on erasure; account anonymization also clears account preferences. Data exports
include stored account choices and paginated conversation mutes. Android channel IDs are stable and
created with translated names by the app; system sound/importance choices are never reset. iOS uses
the normal app notification settings. System permission and device settings always take precedence.

The app reads these small settings when opening their editor and on foreground recovery, writes
only explicit changes, and rereads uncertain writes. Main-tab navigation adds no notification
preference requests. All choices persist in the API, not in an account-independent local store.

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
The idempotent `notification preferences` section creates the two preference tables without
rewriting any existing device registration or opt-out. Run migration **before** starting the new
API/worker; readiness rejects the old schema. Deploy both server processes before the updated app.
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
`preferences/mod.rs` defines the public models, `preferences/http.rs` owns request validation and
mobile guards, and `preferences/pg.rs` persists private choices using `preferences/queries.rs`.
Only SQL text is cached; settings and authorization decisions are read afresh.

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

Local validation on **7 November 2026**: **388 Rust tests passed** with
`cargo test --locked --all-targets` and **41 PostgreSQL tests passed** across the eight suites above,
including repeated schema upgrades. Formatting and Clippy with all targets/features and warnings
denied passed. The disposable database, fixtures and generated credentials were removed after
the run. Provider calls were disabled; this does not validate device reception.

On devices, verify background/cold-start delivery, four languages, the actual current first name,
mutual choices, withdrawn grants, resolved alerts, first-period timing, new-session exclusion and
both privacy destinations. A successful FCM request does not prove OS presentation.

Preference regressions cover filtering at enqueue and delivery, private mutes, expiry, discreet names,
muted-period job suppression, blocked/banned participants and ended matches. Concurrent partial edits
preserve unrelated choices; a write waiting behind account erasure cannot recreate deleted settings.
Exports paginate the requesting participant's choices without exposing the partner's mute. Repeated
upgrades preserve existing account preferences and both participants' private choices. The HTTP
inventory and both Postman collections include all four preference routes, for **147 routes** total.

On real devices, also verify cross-device edits, no catch-up after mute expiry, category
sound settings, foreground silence, explicit opt-out preservation and discreet locked-screen copy.
