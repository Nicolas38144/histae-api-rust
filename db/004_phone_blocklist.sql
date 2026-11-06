-- Observations never overwrite operator decisions. Numbers are never stored in plaintext.
CREATE TABLE IF NOT EXISTS phone_blocklist_control (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    key_fingerprint text NOT NULL CHECK (key_fingerprint ~ '^[0-9a-f]{64}$')
);
CREATE TABLE IF NOT EXISTS phone_blocklist_entry (
    id uuid PRIMARY KEY,
    phone_hash text NOT NULL UNIQUE CHECK (phone_hash ~ '^[0-9a-f]{64}$'),
    phone_hint text NOT NULL CHECK (phone_hint ~ '^\+33……[0-9]{4}$'),
    decision text NOT NULL DEFAULT 'automatic' CHECK (decision IN ('automatic','block','allow')),
    decision_until timestamptz,
    version bigint NOT NULL DEFAULT 0 CHECK (version >= 0),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CHECK ((decision='automatic' AND decision_until IS NULL) OR (decision<>'automatic' AND decision_until IS NOT NULL))
);
CREATE TABLE IF NOT EXISTS phone_blocklist_source (
    source text PRIMARY KEY CHECK (source IN ('castle','ip1')),
    last_attempt_at timestamptz,
    last_success_at timestamptz,
    last_error text CHECK (last_error ~ '^[a-z_]{1,64}$'),
    imported_count integer NOT NULL DEFAULT 0 CHECK (imported_count >= 0)
);
INSERT INTO phone_blocklist_source(source) VALUES ('castle'),('ip1') ON CONFLICT DO NOTHING;
CREATE TABLE IF NOT EXISTS phone_blocklist_observation (
    entry_id uuid NOT NULL REFERENCES phone_blocklist_entry(id) ON DELETE CASCADE,
    source text NOT NULL REFERENCES phone_blocklist_source(source),
    last_seen_at timestamptz NOT NULL,
    imported_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (entry_id,source)
);
CREATE INDEX IF NOT EXISTS idx_phone_blocklist_observation_seen ON phone_blocklist_observation(last_seen_at);
CREATE INDEX IF NOT EXISTS idx_phone_blocklist_entry_created ON phone_blocklist_entry(created_at,id);

-- Dedicated sync credentials can only import observations, never change exceptions or read accounts.
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname='histae_phone_sync') THEN
        GRANT SELECT ON phone_blocklist_control,phone_blocklist_entry,phone_blocklist_source,phone_blocklist_observation TO histae_phone_sync;
        GRANT INSERT (id,phone_hash,phone_hint) ON phone_blocklist_entry TO histae_phone_sync;
        GRANT INSERT,UPDATE ON phone_blocklist_observation TO histae_phone_sync;
        GRANT UPDATE ON phone_blocklist_source TO histae_phone_sync;
    END IF;
END $$;
