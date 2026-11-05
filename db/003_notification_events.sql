-- Idempotent upgrade for personalized lifecycle push notifications.
ALTER TABLE match_init ADD COLUMN IF NOT EXISTS photo_reveal_version bigint NOT NULL DEFAULT 0 CHECK (photo_reveal_version >= 0);
ALTER TABLE notification DROP CONSTRAINT IF EXISTS notification_type_check;
ALTER TABLE notification ADD CONSTRAINT notification_type_check CHECK ((type = ANY (ARRAY['new_match'::text, 'new_message'::text, 'profile_liked'::text, 'match_expiring'::text, 'billing_payment_failed'::text, 'continuation_requested'::text, 'conversation_continued'::text, 'photos_revealed'::text, 'first_period_ending'::text, 'data_request_replied'::text, 'new_session'::text])));
CREATE INDEX IF NOT EXISTS idx_notification_match_kind ON notification (user_id, type, (payload->>'match_id'), expires_at);
