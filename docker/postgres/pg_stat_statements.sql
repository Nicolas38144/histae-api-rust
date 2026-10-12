-- Operational statistics, installed separately from the application schema.
-- Initdb runs this on fresh volumes; run it explicitly on existing databases.
CREATE EXTENSION IF NOT EXISTS pg_stat_statements WITH SCHEMA public;
