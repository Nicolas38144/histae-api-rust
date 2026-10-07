-- Development-only reset. Keep the sole superadmin and their own records.
BEGIN;
SET LOCAL search_path = pg_temp, public;
SELECT pg_advisory_xact_lock(86302009);
LOCK TABLE user_account IN SHARE ROW EXCLUSIVE MODE;

DO $$
BEGIN
  IF current_database() <> 'histae-dev' OR
     (SELECT count(*) FROM user_account WHERE role = 'superadmin') <> 1 THEN
    RAISE EXCEPTION 'Expected histae-dev with exactly one superadmin; nothing deleted';
  END IF;
END;
$$;

CREATE TEMP TABLE purge_users ON COMMIT DROP AS
SELECT user_id, phone_number_hash FROM user_account WHERE role <> 'superadmin';
CREATE UNIQUE INDEX ON purge_users (user_id);

-- SQL alone cannot remove S3 objects or Stripe customers. Abort atomically.
DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM user_photo WHERE user_id IN (SELECT user_id FROM purge_users)) THEN
    RAISE EXCEPTION 'A target account owns photos in object storage; nothing deleted';
  END IF;
  IF EXISTS (SELECT 1 FROM billing_customer
             WHERE user_id IN (SELECT user_id FROM purge_users)
               AND stripe_customer_deleted_at IS NULL)
     OR EXISTS (SELECT 1 FROM user_subscription
                WHERE user_id IN (SELECT user_id FROM purge_users)
                  AND provider_subscription_id IS NOT NULL)
     OR EXISTS (SELECT 1 FROM billing_checkout_session
                WHERE user_id IN (SELECT user_id FROM purge_users)
                  AND stripe_session_id IS NOT NULL) THEN
    RAISE EXCEPTION 'A target account has Stripe data; nothing deleted';
  END IF;
END;
$$;

CREATE TEMP TABLE purge_matches ON COMMIT DROP AS
SELECT id FROM match_init
WHERE user1_id IN (SELECT user_id FROM purge_users)
   OR user2_id IN (SELECT user_id FROM purge_users);

CREATE TEMP TABLE purge_notifications ON COMMIT DROP AS
SELECT id FROM notification AS n
WHERE n.user_id IN (SELECT user_id FROM purge_users)
   OR n.payload->>'match_id' IN (SELECT id::text FROM purge_matches);

-- Outbox aggregate IDs are not foreign keys. Remove their jobs and audit rows.
CREATE TEMP TABLE purge_outbox ON COMMIT DROP AS
SELECT event.id FROM outbox_event AS event
WHERE event.aggregate_id IN (SELECT user_id FROM purge_users)
   OR event.aggregate_id IN (SELECT id FROM purge_matches)
   OR event.aggregate_id IN (SELECT id FROM billing_checkout_session
                             WHERE user_id IN (SELECT user_id FROM purge_users))
   OR event.aggregate_id IN (SELECT id FROM data_subject_request
                             WHERE user_id IN (SELECT user_id FROM purge_users))
   OR event.aggregate_id IN (SELECT id FROM notification_push_delivery
                             WHERE notification_id IN (SELECT id FROM purge_notifications));

DELETE FROM outbox_operator_action
WHERE outbox_event_id IN (SELECT id FROM purge_outbox)
   OR administrator_id IN (SELECT user_id FROM purge_users);
DELETE FROM outbox_event WHERE id IN (SELECT id FROM purge_outbox);
DELETE FROM notification WHERE id IN (SELECT id FROM purge_notifications);
DELETE FROM data_access_log
WHERE accessed_user_id IN (SELECT user_id FROM purge_users)
   OR accessor_id IN (SELECT user_id FROM purge_users);
DELETE FROM billing_invoice
WHERE user_id IN (SELECT user_id FROM purge_users)
   OR stripe_customer_id IN (SELECT stripe_customer_id FROM billing_customer
                             WHERE user_id IN (SELECT user_id FROM purge_users));
DELETE FROM account_tombstone
WHERE phone_number_hash IN (SELECT phone_number_hash FROM purge_users);

DELETE FROM user_account WHERE user_id IN (SELECT user_id FROM purge_users);

DO $$
BEGIN
  IF (SELECT count(*) FROM user_account) <> 1 OR
     (SELECT count(*) FROM user_account WHERE role = 'superadmin') <> 1 THEN
    RAISE EXCEPTION 'Post-purge account check failed; rolled back';
  END IF;
END;
$$;

SELECT count(*) AS deleted_users FROM purge_users;
COMMIT;
