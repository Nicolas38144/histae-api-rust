-- Administrative bootstrap, executed with psql as a PostgreSQL superuser.
-- Modes: install (fresh cluster), access (restore permissions). Run through psql.
-- 001_schema_postgres.sql remains the schema executed by histae_migrate.
\if :{?bootstrap_mode}
\else
\set bootstrap_mode install
\endif
BEGIN;
SET LOCAL log_statement = 'none';
SET LOCAL log_min_error_statement = 'panic';
SET LOCAL password_encryption = 'scram-sha-256';
SELECT set_config('histae.bootstrap_mode', :'bootstrap_mode', true);
DO $$
BEGIN
  IF current_setting('histae.bootstrap_mode') NOT IN ('install', 'access')
    OR current_database() IN ('postgres', 'template0', 'template1')
    OR NOT (SELECT rolsuper FROM pg_roles WHERE rolname = current_user) THEN
    RAISE EXCEPTION 'invalid administrator bootstrap target or mode';
  END IF;
END;
$$;
SELECT :'bootstrap_mode' = 'install' AS create_roles \gset
\if :create_roles
CREATE ROLE histae_migrate LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE
  NOREPLICATION NOBYPASSRLS PASSWORD :'migrate_password';
CREATE ROLE histae_app LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE
  NOREPLICATION NOBYPASSRLS PASSWORD :'app_password';

\endif
-- Administrator-only access policy for existing Histae roles.
DO $$ BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname='histae_phone_sync') THEN
    CREATE ROLE histae_phone_sync NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
  END IF;
END $$;
DO $$ BEGIN
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname='histae_phone_sync'
      AND (rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls))
     OR EXISTS (SELECT 1 FROM pg_auth_members WHERE member='histae_phone_sync'::regrole) THEN
    RAISE EXCEPTION 'conflicting privileged phone sync role';
  END IF;
END $$;
\if :{?phone_sync_password}
SELECT length(:'phone_sync_password') > 0 AS set_phone_password \gset
\if :set_phone_password
ALTER ROLE histae_phone_sync LOGIN PASSWORD :'phone_sync_password';
\endif
\endif
-- Install privileged extensions as the administrator. Their objects remain
-- administrator-owned; the migrator subsequently creates application objects.
CREATE EXTENSION IF NOT EXISTS pgcrypto WITH SCHEMA public;
CREATE EXTENSION IF NOT EXISTS "uuid-ossp" WITH SCHEMA public;
CREATE EXTENSION IF NOT EXISTS pg_trgm WITH SCHEMA public;
CREATE EXTENSION IF NOT EXISTS postgis WITH SCHEMA public;
CREATE EXTENSION IF NOT EXISTS pg_stat_statements WITH SCHEMA public;
-- Refresh SQL definitions after installing the matching server/extension binaries.
-- These updates require the administrator, never the runtime or migration roles.
ALTER EXTENSION pgcrypto UPDATE;
ALTER EXTENSION "uuid-ossp" UPDATE;
ALTER EXTENSION pg_trgm UPDATE;
ALTER EXTENSION postgis UPDATE;
ALTER EXTENSION pg_stat_statements UPDATE;

SELECT current_database() AS database_name \gset
REVOKE ALL ON DATABASE :"database_name" FROM PUBLIC;
GRANT CONNECT ON DATABASE :"database_name" TO histae_migrate, histae_app, histae_phone_sync;
-- This instance is dedicated to Histae. Runtime roles cannot use another DB
-- to obtain the default PUBLIC CONNECT/TEMPORARY privileges.
REVOKE CONNECT, TEMPORARY ON DATABASE postgres FROM PUBLIC;
REVOKE CONNECT, TEMPORARY ON DATABASE template1 FROM PUBLIC;

ALTER SCHEMA public OWNER TO histae_migrate;
REVOKE ALL ON SCHEMA public FROM PUBLIC;
GRANT USAGE ON SCHEMA public TO histae_app;
GRANT USAGE ON SCHEMA public TO histae_phone_sync;
ALTER ROLE histae_phone_sync SET search_path = public, pg_catalog;
DO $$ BEGIN
  IF to_regclass('public.phone_blocklist_entry') IS NOT NULL THEN
    GRANT SELECT ON phone_blocklist_control,phone_blocklist_entry,phone_blocklist_observation,phone_blocklist_source TO histae_phone_sync;
    GRANT INSERT(id,phone_hash,phone_hint) ON phone_blocklist_entry TO histae_phone_sync;
    GRANT INSERT,UPDATE ON phone_blocklist_observation TO histae_phone_sync;
    GRANT UPDATE ON phone_blocklist_source TO histae_phone_sync;
  END IF;
END $$;
GRANT SELECT ON public.spatial_ref_sys TO histae_migrate, histae_app;

ALTER DEFAULT PRIVILEGES FOR ROLE histae_migrate IN SCHEMA public
  GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO histae_app;
ALTER DEFAULT PRIVILEGES FOR ROLE histae_migrate IN SCHEMA public
  GRANT USAGE, SELECT ON SEQUENCES TO histae_app;
ALTER DEFAULT PRIVILEGES FOR ROLE histae_migrate
  REVOKE EXECUTE ON FUNCTIONS FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE histae_migrate IN SCHEMA public
  GRANT EXECUTE ON FUNCTIONS TO histae_app;

ALTER ROLE histae_migrate SET search_path = public, pg_catalog;
ALTER ROLE histae_app SET search_path = public, pg_catalog;

COMMIT;
