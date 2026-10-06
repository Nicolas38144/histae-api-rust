-- Destructive reset of the connected database's public schema.
-- Removes all tables, views, functions, triggers, types, and extensions in public.
-- Run manually only after selecting and backing up the intended database.
BEGIN;
DROP SCHEMA IF EXISTS public CASCADE;
CREATE SCHEMA public AUTHORIZATION pg_database_owner;
GRANT USAGE ON SCHEMA public TO PUBLIC;
COMMIT;
