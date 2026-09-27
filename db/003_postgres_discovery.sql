-- PostgreSQL is the canonical store for immutable discovery decisions.
CREATE TABLE swipe_decision (
    actor_id uuid NOT NULL,
    target_id uuid NOT NULL,
    decision text NOT NULL,
    swiped_at timestamp with time zone DEFAULT now() NOT NULL,
    expires_at timestamp with time zone DEFAULT (now() + '365 days'::interval) NOT NULL,
    CONSTRAINT swipe_decision_pkey PRIMARY KEY (actor_id, target_id),
    CONSTRAINT swipe_decision_actor_id_fkey FOREIGN KEY (actor_id) REFERENCES user_account(user_id) ON DELETE CASCADE,
    CONSTRAINT swipe_decision_target_id_fkey FOREIGN KEY (target_id) REFERENCES user_account(user_id) ON DELETE CASCADE,
    CONSTRAINT swipe_decision_distinct_users_check CHECK (actor_id <> target_id),
    CONSTRAINT swipe_decision_decision_check CHECK (decision = ANY (ARRAY['like'::text, 'pass'::text])),
    CONSTRAINT swipe_decision_retention_check CHECK (expires_at = swiped_at + '365 days'::interval)
);

COMMENT ON TABLE swipe_decision IS 'Canonical immutable swipe decisions retained for 365 days.';

-- Incoming references are used only for account erasure. Exact reciprocal
-- lookups and feed exclusions use the primary key in actor/target order.
CREATE INDEX idx_swipe_decision_target_actor ON swipe_decision USING btree (target_id, actor_id);

-- Keyset export of one actor's decisions without sorting the whole history.
CREATE INDEX idx_swipe_decision_actor_swiped_target
    ON swipe_decision USING btree (actor_id, swiped_at, target_id)
    INCLUDE (decision, expires_at);

-- Daily bounded retention cleanup.
CREATE INDEX idx_swipe_decision_expires
    ON swipe_decision USING btree (expires_at, actor_id, target_id);

CREATE TRIGGER trg_live_swipe BEFORE INSERT OR UPDATE ON swipe_decision
    FOR EACH ROW EXECUTE FUNCTION fct_require_live_account('actor_id', 'target_id');

-- A workflow already stopped at the former external-store checkpoint can
-- safely resume by deleting the corresponding PostgreSQL rows in batches.
ALTER TABLE account_erasure DROP CONSTRAINT account_erasure_step_check;
ALTER TABLE account_erasure DROP CONSTRAINT account_erasure_scylla_partition_check;
UPDATE account_erasure SET step = 'swipes' WHERE step = 'scylla';
ALTER TABLE account_erasure DROP COLUMN scylla_partition;
ALTER TABLE account_erasure ADD CONSTRAINT account_erasure_step_check
    CHECK (step = ANY (ARRAY['stripe'::text, 'photos'::text, 'swipes'::text, 'postgres'::text, 'completed'::text]));
