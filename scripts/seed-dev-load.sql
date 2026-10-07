-- Run only through seed-dev-load.sh against the local histae-dev database.
-- All generated identifiers are deterministic. No OTP, SMS, Stripe or storage API is called.
BEGIN;
SET LOCAL search_path = pg_temp, public;

DO $$
BEGIN
  IF current_database() <> 'histae-dev' THEN
    RAISE EXCEPTION 'Development load seed requires histae-dev';
  END IF;
END;
$$;

SELECT pg_advisory_xact_lock(86302007);

CREATE TEMP TABLE seed_config ON COMMIT DROP AS
SELECT CAST(:'seed_count' AS integer) AS user_count,
       :'terms_version'::text AS terms_version,
       :'privacy_version'::text AS privacy_version,
       :'sensitive_version'::text AS sensitive_version,
       :'location_version'::text AS location_version;

DO $$
BEGIN
  IF (SELECT user_count FROM seed_config) NOT BETWEEN 5000 AND 10000
     OR (SELECT user_count FROM seed_config) % 100 <> 0 THEN
    RAISE EXCEPTION 'Seed count must be between 5000 and 10000 and divisible by 100';
  END IF;
  IF (SELECT count(*) FROM trait) < 3
     OR (SELECT count(*) FROM profile_question) < 2
     OR NOT EXISTS (SELECT 1 FROM subscription_plan WHERE code = 'free')
     OR NOT EXISTS (SELECT 1 FROM subscription_plan WHERE code = 'premium') THEN
    RAISE EXCEPTION 'Run the development database migration and catalogue seed first';
  END IF;
END;
$$;

-- Centre on the sole superadmin when a location exists, otherwise central Paris.
CREATE TEMP TABLE seed_center ON COMMIT DROP AS
SELECT COALESCE((
         SELECT presence.latitude FROM user_presence AS presence
         JOIN user_account AS account USING (user_id)
         WHERE account.role = 'superadmin' AND account.deleted_at IS NULL
         LIMIT 1
       ), 48.856600::numeric) AS latitude,
       COALESCE((
         SELECT presence.longitude FROM user_presence AS presence
         JOIN user_account AS account USING (user_id)
         WHERE account.role = 'superadmin' AND account.deleted_at IS NULL
         LIMIT 1
       ), 2.352200::numeric) AS longitude;

CREATE TEMP TABLE seed_users ON COMMIT DROP AS
SELECT number AS seed_number,
       uuid_generate_v5('2fc9a04d-bca3-4fa7-b6a1-a27736103699'::uuid,
                        'histae-load-user-' || number) AS user_id
FROM seed_config CROSS JOIN LATERAL generate_series(1, user_count) AS generated(number);
CREATE UNIQUE INDEX ON seed_users (seed_number);
CREATE UNIQUE INDEX ON seed_users (user_id);

DO $$
BEGIN
  IF EXISTS (
    SELECT 1 FROM seed_users AS seed
    JOIN user_account AS account USING (user_id)
    WHERE account.role <> 'user' OR account.is_banned OR account.deleted_at IS NOT NULL
  ) THEN
    RAISE EXCEPTION 'A generated account was changed; refusing to overwrite it';
  END IF;
END;
$$;

INSERT INTO user_account
  (user_id, role, phone_number_hash, phone_number_encrypted, created_at)
SELECT user_id, 'user',
       encode(digest('histae-load-phone-' || seed_number, 'sha256'), 'hex'),
       digest('histae-load-unusable-phone-' || seed_number, 'sha256'),
       now() - make_interval(days => seed_number % 120)
FROM seed_users
ON CONFLICT (user_id) DO NOTHING;

-- Synthetic names and bios are deliberately identifiable in the dashboard.
INSERT INTO user_profile (user_id, firstname, birthdate, sex, bio)
SELECT user_id,
       (ARRAY['Alice', 'Camille', 'Emma', 'Jade', 'Lina', 'Louise',
              'Alex', 'Hugo', 'Jules', 'Lucas', 'Noah', 'Sacha'])
         [1 + (seed_number % 12)] || ' Test ' || lpad(seed_number::text, 5, '0'),
       (current_date - make_interval(years => 20 + seed_number % 36,
                                      days => (seed_number * 13) % 365))::date,
       CASE WHEN seed_number % 2 = 0 THEN 'female' ELSE 'male' END,
       'Profil fictif de charge. J''aime les balades, les échanges sincères et découvrir de nouveaux endroits.'
FROM seed_users
ON CONFLICT (user_id) DO UPDATE SET sex = EXCLUDED.sex;

INSERT INTO user_preferences (user_id, min_age, max_age, max_distance_km, looking_for)
SELECT user_id, 18, 75, 15, 'both' FROM seed_users
ON CONFLICT (user_id) DO NOTHING;

-- All profiles lie within roughly one kilometre of the selected centre.
INSERT INTO user_presence (user_id, latitude, longitude, is_location_fresh, updated_at)
SELECT user_id,
       center.latitude + ((seed_number % 101) - 50) * 0.0001,
       center.longitude + (((seed_number * 7) % 101) - 50) * 0.0001,
       true, clock_timestamp()
FROM seed_users CROSS JOIN seed_center AS center
ON CONFLICT (user_id) DO UPDATE SET
  latitude = EXCLUDED.latitude,
  longitude = EXCLUDED.longitude,
  is_location_fresh = true,
  updated_at = EXCLUDED.updated_at;

INSERT INTO user_subscription (user_id, plan, current_period_ends_at)
SELECT user_id,
       CASE WHEN seed_number % 5 = 0 THEN 'premium' ELSE 'free' END,
       CASE WHEN seed_number % 5 = 0 THEN now() + interval '30 days' ELSE NULL END
FROM seed_users
ON CONFLICT (user_id) DO NOTHING;

INSERT INTO user_consent
  (user_id, consent_type, granted, document_version, user_agent)
SELECT seed.user_id, choice.consent_type, true, choice.document_version,
       'histae-development-load-seed/1.0'
FROM seed_users AS seed CROSS JOIN seed_config AS config
CROSS JOIN LATERAL (VALUES
  ('terms_of_service_acceptance', config.terms_version),
  ('privacy_notice_acknowledgement', config.privacy_version),
  ('sensitive_data_consent', config.sensitive_version),
  ('location_consent', config.location_version)
) AS choice(consent_type, document_version)
ON CONFLICT (user_id, consent_type)
  WHERE withdrawn_at IS NULL AND granted = true DO NOTHING;

WITH ranked_traits AS (
  SELECT id, row_number() OVER (ORDER BY name)::integer AS position,
         count(*) OVER ()::integer AS total FROM trait
)
INSERT INTO user_trait (user_id, trait_id)
SELECT seed.user_id, trait.id
FROM seed_users AS seed JOIN ranked_traits AS trait
  ON trait.position IN (1 + (seed.seed_number % trait.total),
                        1 + ((seed.seed_number + 1) % trait.total),
                        1 + ((seed.seed_number + 2) % trait.total))
ON CONFLICT (user_id, trait_id) DO NOTHING;

INSERT INTO content_moderation_case
  (user_id, content_type, bio_user_id, status, reason_codes, policy_version)
SELECT user_id, 'bio', user_id,
       CASE WHEN seed_number % 20 = 0 THEN 'pending'
            WHEN seed_number % 25 = 0 THEN 'rejected' ELSE 'approved' END,
       CASE WHEN seed_number % 20 = 0 THEN ARRAY['legacy_unreviewed']::text[]
            WHEN seed_number % 25 = 0 THEN ARRAY['spam']::text[]
            ELSE ARRAY[]::text[] END,
       'dev_load_v1'
FROM seed_users
ON CONFLICT DO NOTHING;

-- A small subset also exercises profile answers and their moderation state.
CREATE TEMP TABLE seed_questions ON COMMIT DROP AS
SELECT id, position FROM (
  SELECT id, row_number() OVER (ORDER BY display_order, id)::smallint AS position
  FROM profile_question
) AS ranked WHERE position <= 2;

INSERT INTO user_profile_answer (id, user_id, question_id, answer, position)
SELECT uuid_generate_v5('2fc9a04d-bca3-4fa7-b6a1-a27736103699'::uuid,
                        'answer-' || seed.seed_number || '-' || question.position),
       seed.user_id, question.id,
       'Réponse fictive pour tester les profils et la modération du tableau de bord.',
       question.position
FROM seed_users AS seed CROSS JOIN seed_questions AS question
WHERE seed.seed_number % 10 = 0
ON CONFLICT DO NOTHING;

INSERT INTO content_moderation_case
  (user_id, content_type, profile_answer_id, status, reason_codes, policy_version)
SELECT seed.user_id, 'profile_answer', answer.id,
       CASE WHEN seed.seed_number % 40 = 0 THEN 'pending' ELSE 'approved' END,
       CASE WHEN seed.seed_number % 40 = 0 THEN ARRAY['legacy_unreviewed']::text[]
            ELSE ARRAY[]::text[] END,
       'dev_load_v1'
FROM seed_users AS seed JOIN user_profile_answer AS answer ON answer.user_id = seed.user_id
WHERE seed.seed_number % 10 = 0
  AND answer.id = uuid_generate_v5('2fc9a04d-bca3-4fa7-b6a1-a27736103699'::uuid,
                                   'answer-' || seed.seed_number || '-' || answer.position)
ON CONFLICT DO NOTHING;

SELECT count(*) AS generated_users FROM seed_users;

COMMIT;
