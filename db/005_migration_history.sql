CREATE TABLE schema_migrations (
    version text PRIMARY KEY,
    checksum text NOT NULL,
    applied_at timestamp with time zone NOT NULL DEFAULT now()
);

INSERT INTO schema_migrations (version, checksum) VALUES
    ('001_baseline_20260905', '7d33ff78d8094576acc30af275e1426f2feb6333911283ef1f1aadf2f9b8e111'),
    ('017_postgres_discovery', 'f2e656a133d64a08873c86cb9a4dddbc84c4c1704e590851ed63a3a2ac6d1006'),
    ('018_postgres_admin_webauthn_state', '7127cee30dbb61fc967e0864ffbe636a34a18fa44be9ea449ec0b035d9d8c95a');
