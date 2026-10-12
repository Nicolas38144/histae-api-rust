-- Administrative bootstrap, executed with psql as a PostgreSQL superuser.
-- Modes: install (fresh cluster), access (restore permissions after DROP SCHEMA),
-- convert-dev (preserve an existing histae-dev database). Never run with SQLx.
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
  IF current_setting('histae.bootstrap_mode') NOT IN ('install', 'access', 'convert-dev')
    OR current_database() IN ('postgres', 'template0', 'template1')
    OR NOT (SELECT rolsuper FROM pg_roles WHERE rolname = current_user) THEN
    RAISE EXCEPTION 'invalid administrator bootstrap target or mode';
  END IF;
END;
$$;
SELECT :'bootstrap_mode' = 'convert-dev' AS convert_development,
       :'bootstrap_mode' IN ('install', 'convert-dev') AS create_roles \gset
\if :convert_development
-- Called once by configure-dev-postgres-roles.py, inside its transaction.
-- All application clients must be stopped. No row is deleted or rewritten.
DO $$
BEGIN
  IF current_database() <> 'histae-dev'
    OR NOT (SELECT rolsuper FROM pg_roles WHERE rolname = current_user) THEN
    RAISE EXCEPTION 'development administrator required';
  END IF;
  IF EXISTS (SELECT 1 FROM pg_roles
    WHERE rolname IN ('histae_admin', 'histae_migrate', 'histae_app')) THEN
    RAISE EXCEPTION 'role separation already configured or conflicting role exists';
  END IF;
  IF EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname = current_database()
    AND pid <> pg_backend_pid() AND backend_type = 'client backend') THEN
    RAISE EXCEPTION 'stop all development database clients before conversion';
  END IF;
END;
$$;
CREATE ROLE histae_admin LOGIN SUPERUSER PASSWORD :'admin_password';

\endif
\if :create_roles
CREATE ROLE histae_migrate LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE
  NOREPLICATION NOBYPASSRLS PASSWORD :'migrate_password';
CREATE ROLE histae_app LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE
  NOREPLICATION NOBYPASSRLS PASSWORD :'app_password';

\endif
-- Administrator-only access policy for existing Histae roles.
-- Install privileged extensions as the administrator. Their objects remain
-- administrator-owned; the migrator subsequently creates application objects.
CREATE EXTENSION IF NOT EXISTS pgcrypto WITH SCHEMA public;
CREATE EXTENSION IF NOT EXISTS "uuid-ossp" WITH SCHEMA public;
CREATE EXTENSION IF NOT EXISTS pg_trgm WITH SCHEMA public;
CREATE EXTENSION IF NOT EXISTS postgis WITH SCHEMA public;
CREATE EXTENSION IF NOT EXISTS pg_stat_statements WITH SCHEMA public;

SELECT current_database() AS database_name \gset
REVOKE ALL ON DATABASE :"database_name" FROM PUBLIC;
GRANT CONNECT ON DATABASE :"database_name" TO histae_migrate, histae_app;
-- This instance is dedicated to Histae. Runtime roles cannot use another DB
-- to obtain the default PUBLIC CONNECT/TEMPORARY privileges.
REVOKE CONNECT, TEMPORARY ON DATABASE postgres FROM PUBLIC;
REVOKE CONNECT, TEMPORARY ON DATABASE template1 FROM PUBLIC;

ALTER SCHEMA public OWNER TO histae_migrate;
REVOKE ALL ON SCHEMA public FROM PUBLIC;
GRANT USAGE ON SCHEMA public TO histae_app;
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

\if :convert_development
DO $$
DECLARE object record;
BEGIN
  FOR object IN SELECT c.oid, c.relkind, c.relname FROM pg_class c
    JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p', 'S', 'v', 'm')
      AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_class'::regclass
        AND d.objid = c.oid AND d.deptype = 'e')
  LOOP
    -- Owned SERIAL/IDENTITY sequences follow ALTER TABLE OWNER automatically;
    -- PostgreSQL rejects changing their owner independently.
    IF object.relkind <> 'S' OR NOT EXISTS (SELECT 1 FROM pg_depend d
      WHERE d.classid = 'pg_class'::regclass AND d.objid = object.oid
        AND d.refclassid = 'pg_class'::regclass AND d.deptype IN ('a', 'i')) THEN
      EXECUTE format('ALTER %s public.%I OWNER TO histae_migrate',
        CASE object.relkind WHEN 'S' THEN 'SEQUENCE' WHEN 'v' THEN 'VIEW'
          WHEN 'm' THEN 'MATERIALIZED VIEW' ELSE 'TABLE' END, object.relname);
    END IF;
    IF object.relkind = 'S' THEN
      EXECUTE format('GRANT USAGE, SELECT ON SEQUENCE public.%I TO histae_app', object.relname);
    ELSE
      EXECUTE format('REVOKE ALL ON TABLE public.%I FROM PUBLIC', object.relname);
      EXECUTE format('GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE public.%I TO histae_app', object.relname);
    END IF;
  END LOOP;
  FOR object IN SELECT p.oid::regprocedure AS signature FROM pg_proc p
    JOIN pg_namespace n ON n.oid = p.pronamespace
    WHERE n.nspname = 'public' AND p.prokind = 'f'
      AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_proc'::regclass
        AND d.objid = p.oid AND d.deptype = 'e')
  LOOP
    EXECUTE format('ALTER FUNCTION %s OWNER TO histae_migrate', object.signature);
    EXECUTE format('REVOKE ALL ON FUNCTION %s FROM PUBLIC', object.signature);
    EXECUTE format('GRANT EXECUTE ON FUNCTION %s TO histae_app', object.signature);
  END LOOP;
END;
$$;
SELECT current_user AS previous_administrator \gset
ALTER ROLE :"previous_administrator" NOLOGIN;
\endif
COMMIT;
