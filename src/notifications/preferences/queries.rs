//! Cache SQL text only; account and match access are evaluated on every request.
use std::sync::LazyLock;

// Checking both participants and blocks also prevents using mutes as an existence oracle.
const MATCH_ACCESS: &str = "SELECT m.id FROM match_init m
    JOIN user_account a ON a.user_id=m.user1_id
    JOIN user_account b ON b.user_id=m.user2_id
    WHERE m.id=$2 AND $1 IN (m.user1_id,m.user2_id)
      AND m.status IN ('active','awaiting_continuation','confirmed')
      AND m.expires_at>clock_timestamp()
      AND a.deleted_at IS NULL AND NOT a.is_banned
      AND b.deleted_at IS NULL AND NOT b.is_banned
      AND NOT EXISTS (SELECT 1 FROM user_block
        WHERE (blocker_id=m.user1_id AND blocked_id=m.user2_id)
           OR (blocker_id=m.user2_id AND blocked_id=m.user1_id))";

pub(super) const READ_PREFERENCES: &str = r#"SELECT COALESCE(p.messages,true) AS messages,
                COALESCE(p.new_matches,true) AS new_matches,
                COALESCE(p.continuations,true) AS continuations,
                COALESCE(p.photos,true) AS photos, COALESCE(p.reminders,true) AS reminders,
                COALESCE(p.account_security,true) AS account_security,
                COALESCE(p.discreet,false) AS discreet
            FROM user_account a LEFT JOIN user_notification_preference p ON p.user_id=a.user_id
            WHERE a.user_id=$1 AND a.deleted_at IS NULL AND NOT a.is_banned"#;

// The account lock serializes this write with bans and erasure, including when
// this statement started before their commit. A stale snapshot cannot recreate data.
pub(super) const UPDATE_PREFERENCES: &str = r#"WITH account AS MATERIALIZED (
                SELECT user_id FROM user_account
                WHERE user_id=$1 AND deleted_at IS NULL AND NOT is_banned FOR SHARE
            )
            INSERT INTO user_notification_preference
                (user_id,messages,new_matches,continuations,photos,reminders,account_security,discreet)
            SELECT user_id,COALESCE($2,true),COALESCE($3,true),COALESCE($4,true),
                COALESCE($5,true),COALESCE($6,true),COALESCE($7,true),COALESCE($8,false)
            FROM account WHERE true
            ON CONFLICT (user_id) DO UPDATE SET
                messages=COALESCE($2,user_notification_preference.messages),
                new_matches=COALESCE($3,user_notification_preference.new_matches),
                continuations=COALESCE($4,user_notification_preference.continuations),
                photos=COALESCE($5,user_notification_preference.photos),
                reminders=COALESCE($6,user_notification_preference.reminders),
                account_security=COALESCE($7,user_notification_preference.account_security),
                discreet=COALESCE($8,user_notification_preference.discreet)
            RETURNING messages,new_matches,continuations,photos,reminders,account_security,discreet"#;

pub(super) static READ_MUTE: LazyLock<String> = LazyLock::new(|| {
    format!(
        r#"SELECT
            COALESCE(p.muted AND (p.muted_until IS NULL OR p.muted_until>clock_timestamp()),false) AS muted,
            CASE WHEN p.muted AND p.muted_until>clock_timestamp() THEN p.muted_until END AS muted_until
            FROM ({MATCH_ACCESS}) m LEFT JOIN match_notification_preference p
              ON p.match_id=m.id AND p.user_id=$1"#
    )
});

pub(super) static WRITE_MUTE: LazyLock<String> = LazyLock::new(|| {
    format!(
        r#"INSERT INTO match_notification_preference
            (user_id,match_id,muted,muted_until)
            SELECT $1,m.id,$3,$4 FROM ({MATCH_ACCESS}) m WHERE true
            ON CONFLICT (match_id,user_id) DO UPDATE SET muted=EXCLUDED.muted,
                muted_until=EXCLUDED.muted_until RETURNING muted,muted_until"#
    )
});
