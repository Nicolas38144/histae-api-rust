-- =========================================
-- INITIAL SUBSCRIPTION PLAN CATALOG
-- Run after schema_postgres.sql.
-- =========================================
INSERT INTO subscription_plan (
  code, display_name, monthly_price_cents, annual_price_cents, currency, weekly_continuation_limit
) VALUES ('free', 'Free', 0, 0, 'EUR', 3)
ON CONFLICT (code) DO NOTHING;

INSERT INTO subscription_plan (
  code, display_name, monthly_price_cents, annual_price_cents, currency, weekly_continuation_limit, trial_days
) VALUES ('premium', 'Premium', 500, 3000, 'EUR', NULL, 30)
ON CONFLICT (code) DO NOTHING;

INSERT INTO subscription_plan_feature (
  plan_code, feature_code, display_name, description, feature_value, sort_order
) VALUES
  ('free', 'messages', 'Messaging', 'Message participants while the match remains open.', '{"included":true}'::jsonb, 0),
  ('free', 'photo_reveal', 'Mutual photo reveal', 'Reveal profile photos after both participants agree.', '{"included":true}'::jsonb, 1),
  ('free', 'continuations', '3 continuations per week', 'Three successful match continuations each week.', '{"limit":3}'::jsonb, 2),
  ('premium', 'messages', 'Messaging', 'Message participants while the match remains open.', '{"included":true}'::jsonb, 0),
  ('premium', 'photo_reveal', 'Mutual photo reveal', 'Reveal profile photos after both participants agree.', '{"included":true}'::jsonb, 1),
  ('premium', 'continuations', 'Unlimited continuations', 'Continue matches without a weekly limit.', '{"limit":"unlimited"}'::jsonb, 2)
ON CONFLICT (plan_code, feature_code) DO NOTHING;

-- =========================================
-- INITIAL PERSONALITY TRAIT CATALOG
-- =========================================
INSERT INTO trait (name) VALUES
  ('Curieux'),
  ('Bienveillant'),
  ('Créatif'),
  ('Aventurier'),
  ('À l''écoute'),
  ('Ambitieux'),
  ('Drôle'),
  ('Calme'),
  ('Sociable'),
  ('Empathique'),
  ('Optimiste'),
  ('Passionné'),
  ('Authentique'),
  ('Sportif'),
  ('Gourmand')
ON CONFLICT (name) DO NOTHING;

-- Initial catalogue moved from the former migration 005.
INSERT INTO profile_question(id, code, prompt, category, display_order) VALUES
  ('51000000-0000-4000-8000-000000000001', 'ideal_sunday', 'À quoi ressemble ton dimanche idéal ?', 'daily_life', 10),
  ('51000000-0000-4000-8000-000000000002', 'talk_for_hours', 'De quoi pourrais-tu parler pendant des heures ?', 'conversation', 20),
  ('51000000-0000-4000-8000-000000000003', 'small_joy', 'Quel petit plaisir améliore toujours ta journée ?', 'daily_life', 30),
  ('51000000-0000-4000-8000-000000000004', 'make_me_laugh', 'Qu’est-ce qui te fait rire à tous les coups ?', 'personality', 40),
  ('51000000-0000-4000-8000-000000000005', 'ideal_date', 'Quel serait ton rendez-vous idéal ?', 'relationships', 50),
  ('51000000-0000-4000-8000-000000000006', 'want_to_learn', 'Qu’aimerais-tu apprendre prochainement ?', 'interests', 60),
  ('51000000-0000-4000-8000-000000000007', 'valued_quality', 'Quelle qualité apprécies-tu le plus chez les autres ?', 'relationships', 70),
  ('51000000-0000-4000-8000-000000000008', 'useless_talent', 'Quel est ton talent le plus inutile ?', 'personality', 80),
  ('51000000-0000-4000-8000-000000000009', 'recent_surprise', 'Quelle est la dernière chose qui t’a agréablement surpris·e ?', 'conversation', 90),
  ('51000000-0000-4000-8000-000000000010', 'well_get_along_if', 'On s’entendra bien si…', 'relationships', 100),
  ('51000000-0000-4000-8000-000000000011', 'perfect_evening', 'Pour toi, une soirée réussie, c’est quoi ?', 'daily_life', 110),
  ('51000000-0000-4000-8000-000000000012', 'spontaneous_adventure', 'Quelle aventure spontanée serais-tu prêt·e à tenter ?', 'interests', 120),
  ('51000000-0000-4000-8000-000000000013', 'favorite_tradition', 'Quelle tradition aimerais-tu toujours conserver ?', 'daily_life', 130),
  ('51000000-0000-4000-8000-000000000014', 'current_curiosity', 'Qu’est-ce qui éveille ta curiosité en ce moment ?', 'interests', 140),
  ('51000000-0000-4000-8000-000000000015', 'comfort_food', 'Quel plat te réconforte instantanément ?', 'daily_life', 150) ON CONFLICT DO NOTHING;


-- =========================================
-- 400 COMPLETE FAKE USERS (DEVELOPMENT ONLY)
-- =========================================
-- The rows below are inserted only when the current PostgreSQL transaction
-- defines `histae.seed_fake_users = on`. `pnpm run db:reset` enables this
-- setting exclusively for the local `histae-dev` database, never for production.
--
-- The UUIDs are deterministic, so rerunning the seed updates the same fake
-- users instead of creating duplicates. Phone values are non-reversible seed
-- placeholders and intentionally cannot be used to authenticate through the
-- mobile OTP flow. Fake profiles intentionally have no stored photo.
--
-- All four current legal choices are recorded. There is deliberately no
-- marketing consent because Histae performs no marketing processing.

WITH fake_users AS (
  SELECT
    seed_number,
    uuid_generate_v5(
      '47b99d44-aed4-4b6f-9a2f-66f8655170e1'::uuid,
      'histae-development-fake-user-' || seed_number
    ) AS user_id,
    '+33990' || lpad(seed_number::text, 6, '0') AS fake_phone
  FROM generate_series(1, 400) AS generated(seed_number)
  WHERE current_setting('histae.seed_fake_users', true) = 'on'
)
INSERT INTO user_account (
  user_id,
  role,
  phone_number_hash,
  phone_number_encrypted,
  is_banned,
  banned_at,
  banned_reason,
  banned_by,
  deleted_at,
  anonymized_at,
  created_at
)
SELECT
  user_id,
  'user',
  encode(digest(fake_phone || ':histae-seed-only', 'sha256'), 'hex'),
  digest(fake_phone || ':non-reversible-encrypted-placeholder', 'sha256'),
  false,
  NULL,
  NULL,
  NULL,
  NULL,
  NULL,
  clock_timestamp() - make_interval(days => (400 - seed_number) % 120)
FROM fake_users
ON CONFLICT (user_id) DO UPDATE SET
  role = EXCLUDED.role,
  phone_number_hash = EXCLUDED.phone_number_hash,
  phone_number_encrypted = EXCLUDED.phone_number_encrypted,
  is_banned = false,
  banned_at = NULL,
  banned_reason = NULL,
  banned_by = NULL,
  deleted_at = NULL,
  anonymized_at = NULL;

WITH seed_values AS (
  SELECT
    ARRAY[
      'Alexandre', 'Antoine', 'Arthur', 'Baptiste', 'Clément', 'Enzo', 'Gabriel', 'Hugo', 'Jules', 'Léo',
      'Louis', 'Lucas', 'Mathis', 'Nathan', 'Nicolas', 'Noah', 'Paul', 'Raphaël', 'Thomas', 'Victor'
    ]::text[] AS male_names,
    ARRAY[
      'Alice', 'Ambre', 'Camille', 'Chloé', 'Clara', 'Emma', 'Inès', 'Jade', 'Jeanne', 'Juliette',
      'Léa', 'Lina', 'Louise', 'Manon', 'Nina', 'Rose', 'Sarah', 'Sofia', 'Zoé', 'Élise'
    ]::text[] AS female_names,
    ARRAY['Alix', 'Charlie', 'Claude', 'Dominique', 'Lou', 'Morgan', 'Sacha', 'Sam', 'Yaël', 'Éden']::text[] AS neutral_names,
    ARRAY[
      'Curieux de nature et toujours partant pour découvrir quelque chose de nouveau.',
      'Plutôt calme au quotidien, mais jamais contre une aventure improvisée.',
      'Attaché aux conversations sincères, à l''humour et aux petits plaisirs simples.',
      'Optimiste, sociable et heureux de partager de nouvelles expériences.',
      'Créatif dans l''âme, j''apprécie autant les sorties que les soirées tranquilles.',
      'Passionné par ce que j''entreprends et toujours à l''écoute des autres.',
      'À la recherche d''une belle complicité construite sans se presser.',
      'Un mélange de spontanéité, de bienveillance et de curiosité.',
      'J''aime rire, apprendre et profiter des bons moments à deux.',
      'Authentique et attentionné, je préfère les échanges qui ont du sens.'
    ]::text[] AS bios,
    ARRAY[
      'la randonnée', 'la cuisine', 'les voyages', 'le cinéma', 'la photographie',
      'les concerts', 'la lecture', 'le sport', 'les musées', 'les balades en ville'
    ]::text[] AS interests
), fake_users AS (
  SELECT
    seed_number,
    uuid_generate_v5(
      '47b99d44-aed4-4b6f-9a2f-66f8655170e1'::uuid,
      'histae-development-fake-user-' || seed_number
    ) AS user_id
  FROM generate_series(1, 400) AS generated(seed_number)
  WHERE current_setting('histae.seed_fake_users', true) = 'on'
)
INSERT INTO user_profile (user_id, firstname, birthdate, sex, bio)
SELECT
  fake_users.user_id,
  CASE
    WHEN seed_number <= 24 THEN
      seed_values.neutral_names[1 + ((seed_number - 1) % array_length(seed_values.neutral_names, 1))]
    WHEN seed_number % 2 = 0 THEN
      seed_values.female_names[1 + ((seed_number - 1) % array_length(seed_values.female_names, 1))]
    ELSE
      seed_values.male_names[1 + ((seed_number - 1) % array_length(seed_values.male_names, 1))]
  END || ' ' || lpad(seed_number::text, 3, '0'),
  current_date - make_interval(
    years => 18 + (seed_number % 43),
    days => (seed_number * 17) % 365
  ),
  CASE
    WHEN seed_number <= 24 THEN 'other'
    WHEN seed_number % 2 = 0 THEN 'female'
    ELSE 'male'
  END,
  seed_values.bios[1 + ((seed_number - 1) % array_length(seed_values.bios, 1))]
    || ' J''aime ' || seed_values.interests[1 + ((seed_number * 3 - 1) % array_length(seed_values.interests, 1))] || '.'
FROM fake_users
CROSS JOIN seed_values
ON CONFLICT (user_id) DO UPDATE SET
  firstname = EXCLUDED.firstname,
  birthdate = EXCLUDED.birthdate,
  sex = EXCLUDED.sex,
  bio = EXCLUDED.bio;

WITH fake_users AS (
  SELECT
    seed_number,
    uuid_generate_v5(
      '47b99d44-aed4-4b6f-9a2f-66f8655170e1'::uuid,
      'histae-development-fake-user-' || seed_number
    ) AS user_id
  FROM generate_series(1, 400) AS generated(seed_number)
  WHERE current_setting('histae.seed_fake_users', true) = 'on'
)
INSERT INTO user_preferences (user_id, min_age, max_age, max_distance_km, looking_for)
SELECT
  user_id,
  18,
  70,
  120,
  CASE WHEN seed_number <= 24 THEN 'other' ELSE 'both' END
FROM fake_users
ON CONFLICT (user_id) DO UPDATE SET
  min_age = EXCLUDED.min_age,
  max_age = EXCLUDED.max_age,
  max_distance_km = EXCLUDED.max_distance_km,
  looking_for = EXCLUDED.looking_for;

WITH fake_users AS (
  SELECT
    seed_number,
    uuid_generate_v5(
      '47b99d44-aed4-4b6f-9a2f-66f8655170e1'::uuid,
      'histae-development-fake-user-' || seed_number
    ) AS user_id
  FROM generate_series(1, 400) AS generated(seed_number)
  WHERE current_setting('histae.seed_fake_users', true) = 'on'
), positioned_users AS (
  SELECT
    user_id,
    seed_number,
    CASE (seed_number - 1) / 50
      WHEN 0 THEN 48.856600
      WHEN 1 THEN 45.764000
      WHEN 2 THEN 43.296500
      WHEN 3 THEN 50.629200
      WHEN 4 THEN 43.604700
      WHEN 5 THEN 44.837800
      WHEN 6 THEN 47.218400
      ELSE 48.573400
    END AS base_latitude,
    CASE (seed_number - 1) / 50
      WHEN 0 THEN 2.352200
      WHEN 1 THEN 4.835700
      WHEN 2 THEN 5.369800
      WHEN 3 THEN 3.057300
      WHEN 4 THEN 1.444200
      WHEN 5 THEN -0.579200
      WHEN 6 THEN -1.553600
      ELSE 7.752100
    END AS base_longitude
  FROM fake_users
)
INSERT INTO user_presence (user_id, latitude, longitude, is_location_fresh, updated_at)
SELECT
  user_id,
  base_latitude + ((((seed_number - 1) % 10) - 4.5) * 0.002),
  base_longitude + (((((seed_number - 1) / 10) % 5) - 2) * 0.002),
  true,
  clock_timestamp()
FROM positioned_users
ON CONFLICT (user_id) DO UPDATE SET
  latitude = EXCLUDED.latitude,
  longitude = EXCLUDED.longitude,
  is_location_fresh = true,
  updated_at = EXCLUDED.updated_at;

WITH fake_users AS (
  SELECT
    seed_number,
    uuid_generate_v5(
      '47b99d44-aed4-4b6f-9a2f-66f8655170e1'::uuid,
      'histae-development-fake-user-' || seed_number
    ) AS user_id
  FROM generate_series(1, 400) AS generated(seed_number)
  WHERE current_setting('histae.seed_fake_users', true) = 'on'
)
INSERT INTO user_subscription (user_id, plan, current_period_ends_at, updated_at)
SELECT
  user_id,
  CASE WHEN seed_number % 5 = 0 THEN 'premium' ELSE 'free' END,
  CASE WHEN seed_number % 5 = 0 THEN clock_timestamp() + INTERVAL '30 days' ELSE NULL END,
  clock_timestamp()
FROM fake_users
ON CONFLICT (user_id) DO UPDATE SET
  plan = EXCLUDED.plan,
  current_period_ends_at = EXCLUDED.current_period_ends_at,
  updated_at = EXCLUDED.updated_at;

WITH fake_users AS (
  SELECT
    seed_number,
    uuid_generate_v5(
      '47b99d44-aed4-4b6f-9a2f-66f8655170e1'::uuid,
      'histae-development-fake-user-' || seed_number
    ) AS user_id
  FROM generate_series(1, 400) AS generated(seed_number)
  WHERE current_setting('histae.seed_fake_users', true) = 'on'
), legal_choices(consent_type) AS (
  VALUES
    ('terms_of_service_acceptance'),
    ('privacy_notice_acknowledgement'),
    ('sensitive_data_consent'),
    ('location_consent')
)
INSERT INTO user_consent (
  user_id,
  consent_type,
  granted,
  document_version,
  ip_address,
  user_agent,
  granted_at,
  withdrawn_at
)
SELECT
  fake_users.user_id,
  legal_choices.consent_type,
  true,
  'development-unversioned',
  '192.0.2.' || (1 + ((seed_number - 1) % 250)),
  'histae-postgres-development-seed/1.0',
  clock_timestamp() - make_interval(days => (400 - seed_number) % 120),
  NULL
FROM fake_users
CROSS JOIN legal_choices
ON CONFLICT (user_id, consent_type)
  WHERE withdrawn_at IS NULL AND granted = true
DO UPDATE SET
  document_version = EXCLUDED.document_version,
  ip_address = EXCLUDED.ip_address,
  user_agent = EXCLUDED.user_agent,
  granted_at = EXCLUDED.granted_at,
  withdrawn_at = NULL;

WITH fake_users AS (
  SELECT
    seed_number,
    uuid_generate_v5(
      '47b99d44-aed4-4b6f-9a2f-66f8655170e1'::uuid,
      'histae-development-fake-user-' || seed_number
    ) AS user_id
  FROM generate_series(1, 400) AS generated(seed_number)
  WHERE current_setting('histae.seed_fake_users', true) = 'on'
), ranked_traits AS (
  SELECT
    id,
    row_number() OVER (ORDER BY name)::integer AS position,
    count(*) OVER ()::integer AS trait_count
  FROM trait
)
INSERT INTO user_trait (user_id, trait_id)
SELECT fake_users.user_id, ranked_traits.id
FROM fake_users
JOIN ranked_traits ON ranked_traits.position IN (
  1 + mod(fake_users.seed_number - 1, ranked_traits.trait_count),
  1 + mod(fake_users.seed_number + 4, ranked_traits.trait_count),
  1 + mod(fake_users.seed_number + 9, ranked_traits.trait_count)
)
ON CONFLICT (user_id, trait_id) DO NOTHING;

-- Keep development bios private until reviewed, as the former moderation migration did.
WITH fake_users AS (
  SELECT uuid_generate_v5('47b99d44-aed4-4b6f-9a2f-66f8655170e1'::uuid,
    'histae-development-fake-user-' || seed_number) AS user_id
  FROM generate_series(1, 400) AS generated(seed_number)
  WHERE current_setting('histae.seed_fake_users', true) = 'on'
)
INSERT INTO content_moderation_case (user_id, content_type, bio_user_id, status, reason_codes, policy_version)
SELECT user_id, 'bio', user_id, 'pending', ARRAY['legacy_unreviewed'], 'legacy_import_v1'
FROM user_profile JOIN fake_users USING (user_id)
WHERE bio IS NOT NULL AND btrim(bio) <> ''
ON CONFLICT DO NOTHING;

DO $$
DECLARE
  complete_user_count integer;
BEGIN
  IF current_setting('histae.seed_fake_users', true) = 'on' THEN
    WITH fake_users AS (
      SELECT uuid_generate_v5(
        '47b99d44-aed4-4b6f-9a2f-66f8655170e1'::uuid,
        'histae-development-fake-user-' || seed_number
      ) AS user_id
      FROM generate_series(1, 400) AS generated(seed_number)
    ), consent_counts AS (
      SELECT user_id, count(*) AS consent_count
      FROM user_consent
      WHERE granted = true AND withdrawn_at IS NULL
      GROUP BY user_id
    ), trait_counts AS (
      SELECT user_id, count(*) AS trait_count
      FROM user_trait
      GROUP BY user_id
    )
    SELECT count(*) INTO complete_user_count
    FROM fake_users
    JOIN user_account USING (user_id)
    JOIN user_profile USING (user_id)
    JOIN user_preferences USING (user_id)
    JOIN user_presence USING (user_id)
    JOIN user_subscription USING (user_id)
    JOIN consent_counts USING (user_id)
    JOIN trait_counts USING (user_id)
    WHERE user_account.deleted_at IS NULL
      AND user_account.is_banned = false
      AND user_profile.sex IS NOT NULL
      AND user_profile.bio IS NOT NULL
      AND user_presence.is_location_fresh = true
      AND consent_counts.consent_count = 4
      AND trait_counts.trait_count >= 3;

    IF complete_user_count <> 400 THEN
      RAISE EXCEPTION 'Expected 400 complete fake users, found %', complete_user_count;
    END IF;
  END IF;
END;
$$;
