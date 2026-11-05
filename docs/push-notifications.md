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
read and failed-payment checks remain. New lifecycle/security alerts expire after one day.
FCM/APNs storage is capped at one day and at the notification's earlier deadline, using
[Android TTL and APNs expiration](https://firebase.google.com/docs/cloud-messaging/customize-messages/setting-message-lifespan).
Alerts already accepted by the provider cannot be recalled by these database checks.

New-session alerts exclude the new family, both when scheduling and sending. Refresh rotation does
not create them. Session/account revocation and device removal invalidate pending deliveries.
Names stay outside routing metadata. The client accepts only known kinds and UUID identifiers,
resynchronizes authentication, then rereads the authorized destination. Foreground stays silent.

## Rollout

No new environment variables. Apply the migration with the migration role, then run matching API,
outbox and maintenance versions. Stop the old worker before the new API produces these event kinds.
`db/003_notification_events.sql` extends the type constraint and adds the photo-grant generation;
`db-migrate` applies it automatically to existing databases. Fresh schemas already include it.
Startup rejects a schema that has not been upgraded.

```bash
cargo run --locked --bin db-migrate
```

## Owner verification

API compilation/tests were deliberately not run during this implementation. From the API root:

```bash
cargo fmt
cargo test --locked --lib notifications::
cargo test --locked --lib localization::copy::notification_tests
```

For integration checks, use the guarded local `histae-dev` database with `ENV=development` and
localhost PostgreSQL, after migration. Never point these fixture tests at production.

```bash
cargo test --locked --features postgres-integration --test notifications_postgres
cargo test --locked --features postgres-integration --test matches_postgres --test matches_inactivity_postgres --test privacy_rights_postgres --test mobile_identity
```

On devices, verify background/cold-start delivery, four languages, the actual current first name,
mutual choices, withdrawn grants, resolved alerts, first-period timing, new-session exclusion and
both privacy destinations. A successful FCM request does not prove OS presentation.
