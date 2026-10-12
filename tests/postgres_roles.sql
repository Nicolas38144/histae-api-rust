-- Only executed inside verify-postgres-roles.sh's new isolated database.
BEGIN;
DO $$
DECLARE relation record;
BEGIN
  IF current_user <> 'histae_app' OR current_database() <> 'histae-roles-test' THEN
    RAISE EXCEPTION 'incorrect verification target';
  END IF;
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname IN ('histae_app', 'histae_migrate')
    AND (rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls)) THEN
    RAISE EXCEPTION 'unexpected privileged role';
  END IF;
  IF (SELECT count(*) FROM pg_tables WHERE schemaname = 'public'
    AND tableowner = 'histae_migrate') <> 46 THEN
    RAISE EXCEPTION 'application schema or future table missing';
  END IF;
  IF pg_has_role(current_user, 'histae_migrate', 'MEMBER')
    OR pg_has_role(current_user, 'histae_admin', 'MEMBER')
    OR has_schema_privilege(current_user, 'public', 'CREATE')
    OR has_database_privilege(current_user, current_database(), 'CREATE')
    OR has_database_privilege(current_user, current_database(), 'TEMPORARY')
    OR has_database_privilege(current_user, 'postgres', 'CONNECT')
    OR has_database_privilege(current_user, 'template1', 'CONNECT') THEN
    RAISE EXCEPTION 'unexpected runtime ownership or creation permission';
  END IF;
  FOR relation IN
    SELECT c.oid, c.relowner FROM pg_class c
    JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p')
      AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_class'::regclass
        AND d.objid = c.oid AND d.deptype = 'e')
  LOOP
    IF relation.relowner <> 'histae_migrate'::regrole
      OR NOT has_table_privilege(current_user, relation.oid, 'SELECT')
      OR NOT has_table_privilege(current_user, relation.oid, 'INSERT')
      OR NOT has_table_privilege(current_user, relation.oid, 'UPDATE')
      OR NOT has_table_privilege(current_user, relation.oid, 'DELETE')
      OR has_table_privilege(current_user, relation.oid, 'TRUNCATE')
      OR has_table_privilege(current_user, relation.oid, 'TRIGGER')
      OR has_table_privilege(current_user, relation.oid, 'REFERENCES') THEN
      RAISE EXCEPTION 'unexpected application table privileges';
    END IF;
  END LOOP;
  FOR relation IN SELECT c.oid, c.relowner FROM pg_class c
    JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE n.nspname = 'public' AND c.relkind = 'S'
      AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_class'::regclass
        AND d.objid = c.oid AND d.deptype = 'e')
  LOOP
    IF relation.relowner <> 'histae_migrate'::regrole
      OR NOT has_sequence_privilege(current_user, relation.oid, 'USAGE')
      OR NOT has_sequence_privilege(current_user, relation.oid, 'SELECT')
      OR has_sequence_privilege(current_user, relation.oid, 'UPDATE') THEN
      RAISE EXCEPTION 'unexpected application sequence privileges';
    END IF;
  END LOOP;
  IF has_table_privilege(current_user, 'public.spatial_ref_sys', 'UPDATE') THEN
    RAISE EXCEPTION 'extension data must not be writable';
  END IF;
  IF EXISTS (SELECT 1 FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
    WHERE n.nspname = 'public' AND p.proname LIKE 'fct_%'
    AND (p.proowner <> 'histae_migrate'::regrole OR p.prosecdef
      OR NOT has_function_privilege(current_user, p.oid, 'EXECUTE'))) THEN
    RAISE EXCEPTION 'unexpected application function privileges';
  END IF;
END;
$$;

INSERT INTO public.role_permission_probe VALUES (1, 'created');
UPDATE public.role_permission_probe SET value = 'updated' WHERE id = 1;
SELECT value, public.role_function_probe(), nextval('public.role_sequence_probe')
FROM public.role_permission_probe;
DELETE FROM public.role_permission_probe;

DO $$
DECLARE command text;
BEGIN
  FOREACH command IN ARRAY ARRAY[
    'CREATE TABLE public.forbidden_table (id integer)',
    'CREATE TEMPORARY TABLE forbidden_temp (id integer)',
    'ALTER TABLE public.role_permission_probe ADD COLUMN forbidden integer',
    'DROP TABLE public.role_permission_probe',
    'TRUNCATE public.role_permission_probe',
    'SET ROLE histae_migrate',
    'SET ROLE histae_admin',
    'CREATE ROLE forbidden_role',
    'COPY (SELECT 1) TO ''/tmp/forbidden-role-write'''
  ] LOOP
    BEGIN
      EXECUTE command;
      RAISE EXCEPTION 'a forbidden operation succeeded';
    EXCEPTION WHEN insufficient_privilege THEN
      NULL;
    END;
  END LOOP;
END;
$$;

-- Real application constraints, trigger functions and PostGIS calls.
INSERT INTO user_account (user_id, phone_number_hash, phone_number_encrypted) VALUES
  ('87000000-0000-4000-8000-000000000001', 'role-test-one', ''::bytea),
  ('87000000-0000-4000-8000-000000000002', 'role-test-two', ''::bytea);
INSERT INTO user_profile (user_id, firstname, birthdate, sex) VALUES
  ('87000000-0000-4000-8000-000000000001', 'Role One', DATE '1990-01-01', 'male'),
  ('87000000-0000-4000-8000-000000000002', 'Role Two', DATE '1990-01-01', 'female');
INSERT INTO user_consent (user_id, consent_type, granted, document_version) VALUES
  ('87000000-0000-4000-8000-000000000001', 'sensitive_data_consent', true, 'role-test-v1'),
  ('87000000-0000-4000-8000-000000000001', 'location_consent', true, 'role-test-v1');
INSERT INTO user_presence (user_id, latitude, longitude, is_location_fresh) VALUES
  ('87000000-0000-4000-8000-000000000001', 48.85, 2.35, true),
  ('87000000-0000-4000-8000-000000000002', 48.851, 2.351, true);
SELECT public.st_dwithin(
  public.st_setsrid(public.st_makepoint(2.35, 48.85), 4326)::public.geography,
  public.st_setsrid(public.st_makepoint(2.351, 48.851), 4326)::public.geography,
  1000, false);
INSERT INTO swipe_decision (actor_id, target_id, decision, swiped_at, expires_at)
VALUES ('87000000-0000-4000-8000-000000000001',
  '87000000-0000-4000-8000-000000000002', 'like', now(), now() + INTERVAL '365 days');
INSERT INTO match_init (id, user1_id, user2_id) VALUES
  ('87000000-0000-4000-8000-000000000003',
   '87000000-0000-4000-8000-000000000001', '87000000-0000-4000-8000-000000000002');
INSERT INTO chat_message (match_id, sender_id, content) VALUES
  ('87000000-0000-4000-8000-000000000003', '87000000-0000-4000-8000-000000000001',
   'A message from the limited application role');
SELECT fct_anonymize_user('87000000-0000-4000-8000-000000000001');
DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM user_presence
    WHERE user_id = '87000000-0000-4000-8000-000000000001')
    OR NOT EXISTS (SELECT 1 FROM user_account
      WHERE user_id = '87000000-0000-4000-8000-000000000001'
        AND anonymized_at IS NOT NULL AND phone_number_encrypted = ''::bytea) THEN
    RAISE EXCEPTION 'account erasure did not complete';
  END IF;
  IF NOT EXISTS (SELECT 1 FROM chat_message
    WHERE sender_id = '87000000-0000-4000-8000-000000000001'
      AND content = '[Message supprimé]') THEN
    RAISE EXCEPTION 'message erasure did not complete';
  END IF;
END;
$$;
ROLLBACK;
