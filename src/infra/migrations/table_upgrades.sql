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

-- BEGIN free text storage
-- Relax only previously tighter storage bounds. Keep historical text readable.
DO $$
DECLARE
    item record;
BEGIN
    FOR item IN SELECT * FROM (VALUES
        ('profile_question'::regclass, 'chk_profile_question_prompt', 'prompt',
         'prompt = btrim(prompt) AND char_length(prompt) BETWEEN 3 AND 1000'),
        ('user_profile_answer'::regclass, 'chk_user_profile_answer_text', 'answer',
         'answer = btrim(answer) AND char_length(answer) BETWEEN 10 AND 1000'),
        ('content_moderation_case'::regclass, 'chk_content_moderation_review_reason', 'review_reason',
         'review_reason IS NULL OR (review_reason = btrim(review_reason) AND char_length(review_reason) BETWEEN 3 AND 1000)'),
        ('outbox_event'::regclass, 'chk_outbox_event_resolution', 'resolution_reason',
         '(status = ''discarded'' AND resolved_at IS NOT NULL AND resolution_reason IS NOT NULL
      AND char_length(resolution_reason) BETWEEN 3 AND 1000)
      OR (status <> ''discarded'' AND resolved_at IS NULL AND resolved_by IS NULL AND resolution_reason IS NULL)'),
        ('outbox_operator_action'::regclass, 'outbox_operator_action_reason_check', 'reason',
         'char_length(reason) BETWEEN 3 AND 1000'),
        ('profile_question_translation'::regclass, 'profile_question_translation_prompt_check', 'prompt',
         'char_length(prompt) BETWEEN 3 AND 1000')
    ) AS rules(relation, constraint_name, column_name, expression)
    LOOP
        IF NOT EXISTS (
            SELECT 1 FROM pg_constraint
            WHERE conrelid = item.relation AND conname = item.constraint_name
              AND strpos(pg_get_constraintdef(oid),
                  format('char_length(%I) <= 1000', item.column_name)) > 0
        ) THEN
            EXECUTE format('ALTER TABLE %s DROP CONSTRAINT IF EXISTS %I, ADD CONSTRAINT %I CHECK (%s)',
                item.relation, item.constraint_name, item.constraint_name, item.expression);
        END IF;
    END LOOP;

    -- PostgreSQL B-tree entries have a byte limit. Keep case-insensitive
    -- uniqueness for long multibyte prompts using a fixed-size digest key.
    IF COALESCE(pg_get_indexdef(to_regclass('uq_profile_question_prompt_ci')), '') NOT LIKE '%digest(%' THEN
        DROP INDEX IF EXISTS uq_profile_question_prompt_ci;
        CREATE UNIQUE INDEX uq_profile_question_prompt_ci
          ON profile_question (public.digest(lower(prompt), 'sha256'));
    END IF;
END;
$$;
-- END free text storage
