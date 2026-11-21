-- Upgrades for existing databases only. Fresh definitions live in db/001_schema_postgres.sql.
-- Both paths are compared against PostgreSQL by the migration integration tests.

-- BEGIN play review account
-- Server-created review identities must never obtain an administrative role.
ALTER TABLE user_account ADD COLUMN IF NOT EXISTS is_review_account boolean NOT NULL DEFAULT false;
DO $$ BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_constraint WHERE conrelid='user_account'::regclass
      AND conname='user_account_review_role_check'
  ) THEN
    ALTER TABLE user_account ADD CONSTRAINT user_account_review_role_check
      CHECK (NOT is_review_account OR role='user');
  END IF;
END $$;
-- END play review account

-- BEGIN event identifiers
-- Defaults affect new rows only; existing UUIDs, references and timestamps stay intact.
ALTER TABLE chat_message ALTER COLUMN id SET DEFAULT uuidv7();
ALTER TABLE notification ALTER COLUMN id SET DEFAULT uuidv7();
ALTER TABLE outbox_event ALTER COLUMN id SET DEFAULT uuidv7();
-- END event identifiers

-- BEGIN notification events
ALTER TABLE match_init ADD COLUMN IF NOT EXISTS photo_reveal_version bigint NOT NULL DEFAULT 0 CHECK (photo_reveal_version >= 0);
ALTER TABLE notification DROP CONSTRAINT IF EXISTS notification_type_check;
ALTER TABLE notification ADD CONSTRAINT notification_type_check CHECK ((type = ANY (ARRAY['new_match'::text, 'new_message'::text, 'profile_liked'::text, 'match_expiring'::text, 'continuation_requested'::text, 'conversation_continued'::text, 'photos_revealed'::text, 'first_period_ending'::text, 'data_request_replied'::text, 'new_session'::text])));
-- END notification events

-- BEGIN store billing columns
ALTER TABLE IF EXISTS billing_store_subscription ADD COLUMN IF NOT EXISTS superseded boolean NOT NULL DEFAULT false;
-- END store billing columns

-- BEGIN report responses
ALTER TABLE user_report ADD COLUMN IF NOT EXISTS public_response text
    CONSTRAINT user_report_public_response_check
    CHECK (public_response IS NULL OR char_length(public_response) BETWEEN 1 AND 2000);
-- END report responses

