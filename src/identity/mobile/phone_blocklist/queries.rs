//! SQL projections and writes for phone policy; never project phone hashes to HTTP.

pub(super) const EXCLUSIVE_LOCK: &str = "SELECT pg_advisory_xact_lock($1)";
pub(super) const SHARED_LOCK: &str = "SELECT pg_advisory_xact_lock_shared($1)";
pub(super) const INITIALIZE_KEY: &str = "INSERT INTO phone_blocklist_control(singleton,key_fingerprint) VALUES(true,$1) ON CONFLICT(singleton) DO NOTHING";
pub(super) const READ_KEY: &str =
    "SELECT key_fingerprint FROM phone_blocklist_control WHERE singleton";
pub(super) const CHECK_POLICY: &str = r#"
SELECT COALESCE(e.decision='allow' AND e.decision_until>clock_timestamp(),false),
 CASE WHEN e.decision_until>clock_timestamp() THEN e.decision='block' ELSE
   EXISTS(SELECT 1 FROM phone_blocklist_observation o WHERE o.entry_id=e.id
          AND o.last_seen_at>clock_timestamp()-INTERVAL '90 days') END,
 EXISTS(SELECT 1 FROM phone_blocklist_source WHERE last_success_at>clock_timestamp()-INTERVAL '7 days')
FROM (VALUES(1)) placeholder(value) LEFT JOIN phone_blocklist_entry e ON e.phone_hash=$1
"#;
pub(super) const SOURCES: &str = "SELECT source,last_attempt_at,last_success_at,last_error,imported_count FROM phone_blocklist_source ORDER BY source";
pub(super) const READY: &str = "SELECT EXISTS(SELECT 1 FROM phone_blocklist_source WHERE last_success_at>clock_timestamp()-INTERVAL '7 days')";
pub(super) const COUNT: &str = "SELECT count(*) FROM phone_blocklist_entry";
pub(super) const ENTRY: &str = r#"
SELECT e.id,e.phone_hint,e.decision,e.decision_until,e.version,
 CASE WHEN e.decision_until>clock_timestamp() THEN e.decision='block' ELSE
   EXISTS(SELECT 1 FROM phone_blocklist_observation o WHERE o.entry_id=e.id
          AND o.last_seen_at>clock_timestamp()-INTERVAL '90 days') END AS blocked,
 COALESCE((SELECT jsonb_agg(jsonb_build_object('source',o.source,'last_seen_at',o.last_seen_at,
          'expires_at',o.last_seen_at+INTERVAL '90 days') ORDER BY o.source)
          FROM phone_blocklist_observation o WHERE o.entry_id=e.id),'[]'::jsonb) AS observations
FROM phone_blocklist_entry e
"#;
pub(super) const ACTOR_ROLE: &str = "SELECT role FROM user_account WHERE user_id=$1 AND deleted_at IS NULL AND NOT is_banned FOR SHARE";
pub(super) const INSERT_ENTRY: &str = "INSERT INTO phone_blocklist_entry(id,phone_hash,phone_hint) VALUES($1,$2,$3) ON CONFLICT(phone_hash) DO NOTHING";
pub(super) const ENTRY_ID: &str = "SELECT id FROM phone_blocklist_entry WHERE phone_hash=$1";
pub(super) const LOCK_VERSION: &str =
    "SELECT version FROM phone_blocklist_entry WHERE id=$1 FOR UPDATE";
pub(super) const CHANGE: &str = r#"
UPDATE phone_blocklist_entry SET decision=$2,
 decision_until=CASE WHEN $3::bigint IS NULL THEN NULL ELSE clock_timestamp()+$3*INTERVAL '1 day' END,
 version=version+1,updated_at=clock_timestamp() WHERE id=$1
"#;
pub(super) const AUDIT: &str = "INSERT INTO data_access_log(accessed_user_id,accessor_id,accessor_role,action,reason) VALUES($1,$1,$2,$3,$4)";
pub(super) const IMPORT_ENTRIES: &str = r#"
INSERT INTO phone_blocklist_entry(id,phone_hash,phone_hint)
SELECT * FROM unnest($1::uuid[],$2::text[],$3::text[]) ON CONFLICT(phone_hash) DO NOTHING
"#;
pub(super) const IMPORT_OBSERVATIONS: &str = r#"
INSERT INTO phone_blocklist_observation(entry_id,source,last_seen_at)
SELECT e.id,$1,data.seen FROM unnest($2::text[],$3::timestamptz[]) data(hash,seen)
JOIN phone_blocklist_entry e ON e.phone_hash=data.hash
ON CONFLICT(entry_id,source) DO UPDATE SET
 last_seen_at=GREATEST(phone_blocklist_observation.last_seen_at,EXCLUDED.last_seen_at),
 imported_at=clock_timestamp()
"#;
pub(super) const SOURCE_SUCCEEDED: &str = r#"
UPDATE phone_blocklist_source SET last_attempt_at=clock_timestamp(),last_success_at=clock_timestamp(),
 last_error=NULL,imported_count=$2 WHERE source=$1
"#;
pub(super) const SOURCE_FAILED: &str = "UPDATE phone_blocklist_source SET last_attempt_at=clock_timestamp(),last_error='phone_feed_unavailable' WHERE source=$1";
