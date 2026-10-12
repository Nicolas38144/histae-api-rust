use std::collections::HashSet;

use chrono::{DateTime, TimeDelta, Utc};
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use super::domain::{
    DiscoveryBatch, DiscoveryCandidateRow, DiscoveryCursor, DiscoveryStatusRow, FeedProfileAnswer,
    RecordedSwipe, SWIPE_RETENTION_DAYS, SwipeDecision, SwipeRecord,
};
use crate::infra::postgres::{Database, map_sqlx_error};
use crate::infra::postgres_locks::{AccountActivityPool, ActivityLease};
use crate::profiles::domain::Sex;

use super::store::{DiscoveryRepository, DiscoveryStoreError, DiscoveryStoreFuture, SwipeStore};

#[derive(Clone)]
pub struct PgDiscoveryRepository {
    database: Database,
}

impl PgDiscoveryRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    async fn feed_batch_inner(
        &self,
        user_id: Uuid,
        sensitive_version: &str,
        location_version: &str,
        limit: u32,
        cursor: Option<DiscoveryCursor>,
    ) -> Result<DiscoveryBatch, DiscoveryStoreError> {
        let mut connection = self.database.acquire().await?;
        let query = format!(
            "{VIEWER_SQL}{FEED_GEOGRAPHY_SQL}{GEO_JOIN_SQL}{SPATIAL_FILTER_SQL}{FEED_PAGE_SQL}{FEED_RESULT_SQL_PREFIX}{PAGE_CONTENT_SQL}{FEED_RESULT_SQL_SUFFIX}"
        );
        let mut radius_km = cursor.map_or(0.1, |position| (position.distance_km + 0.1).max(0.1));
        loop {
            let rows = sqlx::query(&query)
                .bind(user_id)
                .bind(sensitive_version)
                .bind(location_version)
                .bind(cursor.map(|position| position.distance_km))
                .bind(cursor.map(|position| position.id))
                .bind(i64::from(limit))
                .bind(Option::<Uuid>::None)
                .bind(radius_km)
                .fetch_all(&mut *connection)
                .await
                .map_err(map_sqlx_error)?;
            let metadata = rows.first().ok_or(DiscoveryStoreError::InvalidStoredData)?;
            let ready: bool = metadata.try_get("viewer_ready").map_err(map_sqlx_error)?;
            let exhausted: bool = metadata
                .try_get("radius_exhausted")
                .map_err(map_sqlx_error)?;
            let candidates = rows
                .iter()
                .filter_map(|row| match row.try_get::<Option<Uuid>, _>("user_id") {
                    Ok(Some(_)) => Some(map_candidate(row)),
                    Ok(None) => None,
                    Err(error) => Some(Err(map_sqlx_error(error).into())),
                })
                .collect::<Result<Vec<_>, _>>()?;
            if !ready || exhausted || candidates.len() >= limit as usize {
                return Ok(DiscoveryBatch { ready, candidates });
            }
            radius_km *= 2.0;
        }
    }
}

impl DiscoveryRepository for PgDiscoveryRepository {
    fn feed_batch(
        &self,
        user_id: Uuid,
        sensitive_version: &str,
        location_version: &str,
        limit: u32,
        cursor: Option<DiscoveryCursor>,
    ) -> DiscoveryStoreFuture<'_, DiscoveryBatch> {
        let sensitive_version = sensitive_version.to_owned();
        let location_version = location_version.to_owned();
        Box::pin(async move {
            self.feed_batch_inner(
                user_id,
                &sensitive_version,
                &location_version,
                limit,
                cursor,
            )
            .await
        })
    }

    fn status(
        &self,
        user_id: Uuid,
        sensitive_version: &str,
        location_version: &str,
    ) -> DiscoveryStoreFuture<'_, DiscoveryStatusRow> {
        let sensitive_version = sensitive_version.to_owned();
        let location_version = location_version.to_owned();
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            let row = sqlx::query(STATUS_SQL)
                .bind(user_id)
                .bind(sensitive_version)
                .bind(location_version)
                .fetch_one(&mut *connection)
                .await
                .map_err(map_sqlx_error)?;
            Ok(DiscoveryStatusRow {
                has_profile: row.try_get("has_profile").map_err(map_sqlx_error)?,
                has_sex: row.try_get("has_sex").map_err(map_sqlx_error)?,
                has_preferences: row.try_get("has_preferences").map_err(map_sqlx_error)?,
                has_sensitive_consent: row
                    .try_get("has_sensitive_consent")
                    .map_err(map_sqlx_error)?,
                has_location_consent: row
                    .try_get("has_location_consent")
                    .map_err(map_sqlx_error)?,
                has_fresh_presence: row.try_get("has_fresh_presence").map_err(map_sqlx_error)?,
                presence_expires_at: row.try_get("presence_expires_at").map_err(map_sqlx_error)?,
            })
        })
    }

    fn is_ready(
        &self,
        user_id: Uuid,
        sensitive_version: &str,
        location_version: &str,
    ) -> DiscoveryStoreFuture<'_, bool> {
        let sensitive_version = sensitive_version.to_owned();
        let location_version = location_version.to_owned();
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            sqlx::query_scalar(READY_SQL)
                .bind(user_id)
                .bind(sensitive_version)
                .bind(location_version)
                .fetch_one(&mut *connection)
                .await
                .map_err(map_sqlx_error)
                .map_err(DiscoveryStoreError::from)
        })
    }

    fn candidate_batch(
        &self,
        user_id: Uuid,
        sensitive_version: &str,
        location_version: &str,
        limit: u32,
        cursor: Option<DiscoveryCursor>,
        target_id: Option<Uuid>,
    ) -> DiscoveryStoreFuture<'_, Vec<DiscoveryCandidateRow>> {
        let sensitive_version = sensitive_version.to_owned();
        let location_version = location_version.to_owned();
        Box::pin(async move {
            if target_id.is_none() {
                return self
                    .feed_batch_inner(
                        user_id,
                        &sensitive_version,
                        &location_version,
                        limit,
                        cursor,
                    )
                    .await
                    .map(|batch| batch.candidates);
            }
            let mut connection = self.database.acquire().await?;
            let query = {
                // Swipe eligibility deliberately ignores previous swipe decisions:
                // the service still needs to resolve retries and expired decisions.
                format!(
                    "{VIEWER_SQL}{TARGET_SETS_SQL}{CANDIDATE_COLUMNS_SQL}{GEO_JOIN_SQL}{CANDIDATE_PAGE_SQL}{PAGE_CONTENT_SQL}"
                )
            };
            let rows = sqlx::query(&query)
                .bind(user_id)
                .bind(sensitive_version)
                .bind(location_version)
                .bind(cursor.map(|value| value.distance_km))
                .bind(cursor.map(|value| value.id))
                .bind(i64::from(limit))
                .bind(target_id)
                .fetch_all(&mut *connection)
                .await
                .map_err(map_sqlx_error)?;
            rows.iter().map(map_candidate).collect()
        })
    }
}

#[derive(Clone)]
pub struct PgSwipeStore {
    database: Database,
    activity: AccountActivityPool,
}

impl PgSwipeStore {
    pub fn new(database: Database, activity: AccountActivityPool) -> Self {
        Self { database, activity }
    }

    async fn record_while_active(
        &self,
        actor_id: Uuid,
        target_id: Uuid,
        decision: SwipeDecision,
        lease: &ActivityLease,
    ) -> Result<RecordedSwipe, DiscoveryStoreError> {
        lease.assert_held()?;
        let now = Utc::now();
        let expires_at = now + TimeDelta::days(SWIPE_RETENTION_DAYS);
        let result = self
            .database
            .transaction(|connection| {
                Box::pin(async move {
                    if let Some(stored) = sqlx::query_scalar::<_, String>(
                        "INSERT INTO swipe_decision
                         (actor_id, target_id, decision, swiped_at, expires_at)
                         VALUES ($1, $2, $3, $4, $5)
                         ON CONFLICT (actor_id, target_id) DO NOTHING
                         RETURNING decision",
                    )
                    .bind(actor_id)
                    .bind(target_id)
                    .bind(decision.as_str())
                    .bind(now)
                    .bind(expires_at)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?
                    {
                        return parsed_recorded(true, &stored);
                    }

                    let row = sqlx::query(
                        "SELECT decision, expires_at FROM swipe_decision
                         WHERE actor_id = $1 AND target_id = $2 FOR UPDATE",
                    )
                    .bind(actor_id)
                    .bind(target_id)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?
                    .ok_or(DiscoveryStoreError::InvalidStoredData)?;
                    let stored: String = row.try_get("decision").map_err(map_sqlx_error)?;
                    let stored_decision = SwipeDecision::parse(&stored)
                        .ok_or(DiscoveryStoreError::InvalidStoredData)?;
                    let stored_expiry: DateTime<Utc> =
                        row.try_get("expires_at").map_err(map_sqlx_error)?;
                    if stored_expiry > now {
                        return Ok(RecordedSwipe {
                            created: false,
                            decision: stored_decision,
                        });
                    }

                    let replaced = sqlx::query_scalar::<_, String>(
                        "UPDATE swipe_decision
                         SET decision = $3, swiped_at = $4, expires_at = $5
                         WHERE actor_id = $1 AND target_id = $2
                         RETURNING decision",
                    )
                    .bind(actor_id)
                    .bind(target_id)
                    .bind(decision.as_str())
                    .bind(now)
                    .bind(expires_at)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(map_sqlx_error)?
                    .ok_or(DiscoveryStoreError::InvalidStoredData)?;
                    parsed_recorded(true, &replaced)
                })
            })
            .await?;
        lease.assert_held()?;
        Ok(result)
    }
}

impl SwipeStore for PgSwipeStore {
    fn record(
        &self,
        actor_id: Uuid,
        target_id: Uuid,
        decision: SwipeDecision,
    ) -> DiscoveryStoreFuture<'_, RecordedSwipe> {
        let activity = self.activity.clone();
        let store = self.clone();
        Box::pin(async move {
            activity
                .run(&[actor_id, target_id], move |lease| {
                    Box::pin(async move {
                        store
                            .record_while_active(actor_id, target_id, decision, lease)
                            .await
                    })
                })
                .await
        })
    }

    fn find(
        &self,
        actor_id: Uuid,
        target_id: Uuid,
    ) -> DiscoveryStoreFuture<'_, Option<SwipeRecord>> {
        Box::pin(async move {
            let mut connection = self.database.acquire().await?;
            let row = sqlx::query(
                "SELECT actor_id, target_id, decision, swiped_at
                 FROM swipe_decision
                 WHERE actor_id = $1 AND target_id = $2 AND expires_at > clock_timestamp()",
            )
            .bind(actor_id)
            .bind(target_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
            row.map(|row| {
                let decision: String = row.try_get("decision").map_err(map_sqlx_error)?;
                Ok(SwipeRecord {
                    actor_id: row.try_get("actor_id").map_err(map_sqlx_error)?,
                    target_id: row.try_get("target_id").map_err(map_sqlx_error)?,
                    decision: SwipeDecision::parse(&decision)
                        .ok_or(DiscoveryStoreError::InvalidStoredData)?,
                    swiped_at: row.try_get("swiped_at").map_err(map_sqlx_error)?,
                })
            })
            .transpose()
        })
    }

    fn swiped_target_ids(
        &self,
        actor_id: Uuid,
        target_ids: Vec<Uuid>,
    ) -> DiscoveryStoreFuture<'_, HashSet<Uuid>> {
        Box::pin(async move {
            if target_ids.is_empty() {
                return Ok(HashSet::new());
            }
            let mut connection = self.database.acquire().await?;
            let ids: Vec<Uuid> = sqlx::query_scalar(
                "SELECT target_id FROM swipe_decision
                 WHERE actor_id = $1 AND target_id = ANY($2::uuid[])
                   AND expires_at > clock_timestamp()",
            )
            .bind(actor_id)
            .bind(target_ids)
            .fetch_all(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
            Ok(ids.into_iter().collect())
        })
    }
}

fn parsed_recorded(created: bool, value: &str) -> Result<RecordedSwipe, DiscoveryStoreError> {
    Ok(RecordedSwipe {
        created,
        decision: SwipeDecision::parse(value).ok_or(DiscoveryStoreError::InvalidStoredData)?,
    })
}

fn map_candidate(
    row: &sqlx::postgres::PgRow,
) -> Result<DiscoveryCandidateRow, DiscoveryStoreError> {
    let sex: String = row.try_get("sex").map_err(map_sqlx_error)?;
    let answers: Value = row.try_get("profile_answers").map_err(map_sqlx_error)?;
    let profile_answers: Vec<FeedProfileAnswer> =
        serde_json::from_value(answers).map_err(|_| DiscoveryStoreError::InvalidStoredData)?;
    Ok(DiscoveryCandidateRow {
        user_id: row.try_get("user_id").map_err(map_sqlx_error)?,
        firstname: row.try_get("firstname").map_err(map_sqlx_error)?,
        age: row.try_get("age").map_err(map_sqlx_error)?,
        sex: Sex::parse(&sex).ok_or(DiscoveryStoreError::InvalidStoredData)?,
        bio: row.try_get("bio").map_err(map_sqlx_error)?,
        distance_km: row.try_get("distance_km").map_err(map_sqlx_error)?,
        traits: row.try_get("traits").map_err(map_sqlx_error)?,
        profile_answers,
    })
}

const STATUS_SQL: &str = r#"
SELECT
  EXISTS (SELECT 1 FROM user_profile WHERE user_id = $1) AS has_profile,
  EXISTS (SELECT 1 FROM user_profile WHERE user_id = $1 AND sex IS NOT NULL) AS has_sex,
  EXISTS (SELECT 1 FROM user_preferences WHERE user_id = $1) AS has_preferences,
  EXISTS (SELECT 1 FROM user_consent WHERE user_id = $1
    AND consent_type = 'sensitive_data_consent' AND granted = true
    AND withdrawn_at IS NULL AND document_version = $2) AS has_sensitive_consent,
  EXISTS (SELECT 1 FROM user_consent WHERE user_id = $1
    AND consent_type = 'location_consent' AND granted = true
    AND withdrawn_at IS NULL AND document_version = $3) AS has_location_consent,
  EXISTS (SELECT 1 FROM user_presence WHERE user_id = $1 AND is_location_fresh = true
    AND updated_at > clock_timestamp() - INTERVAL '1 hour') AS has_fresh_presence,
  (SELECT updated_at + INTERVAL '1 hour' FROM user_presence WHERE user_id = $1) AS presence_expires_at
"#;

const READY_SQL: &str = r#"
SELECT EXISTS (
  SELECT 1 FROM user_account AS account
  JOIN user_profile AS profile ON profile.user_id = account.user_id AND profile.sex IS NOT NULL
  JOIN user_preferences AS preferences ON preferences.user_id = account.user_id
  JOIN user_presence AS presence ON presence.user_id = account.user_id
    AND presence.is_location_fresh = true
    AND presence.updated_at > clock_timestamp() - INTERVAL '1 hour'
  WHERE account.user_id = $1 AND account.deleted_at IS NULL AND account.is_banned = false
    AND EXISTS (SELECT 1 FROM user_consent WHERE user_id = account.user_id
      AND consent_type = 'sensitive_data_consent' AND granted = true
      AND withdrawn_at IS NULL AND document_version = $2)
    AND EXISTS (SELECT 1 FROM user_consent WHERE user_id = account.user_id
      AND consent_type = 'location_consent' AND granted = true
      AND withdrawn_at IS NULL AND document_version = $3)
) AS ready
"#;

// The feed builds exclusion sets once. A targeted swipe checks only its target;
// existing swipe decisions remain eligible for the idempotency check in the service.
const VIEWER_SQL: &str = r#"
WITH viewer AS MATERIALIZED (
  SELECT profile.birthdate, profile.sex,
    date_part('year', age(current_date, profile.birthdate))::integer AS age,
    preferences.min_age, preferences.max_age, preferences.max_distance_km, preferences.looking_for,
    presence.latitude, presence.longitude,
    CASE WHEN presence.latitude BETWEEN -90 AND 90 AND presence.longitude BETWEEN -180 AND 180
      THEN public.st_setsrid(public.st_makepoint(presence.longitude::double precision, presence.latitude::double precision), 4326)::public.geography
      ELSE NULL END AS location,
    preferences.max_distance_km::numeric / 111.0 AS latitude_delta,
    (preferences.max_distance_km::double precision
      / (111.0 * greatest(abs(cos(radians(presence.latitude::double precision))), 0.01)))::numeric
      AS longitude_delta
  FROM user_account AS account
  JOIN user_profile AS profile ON profile.user_id = account.user_id AND profile.sex IS NOT NULL
  JOIN user_preferences AS preferences ON preferences.user_id = account.user_id
  JOIN user_presence AS presence ON presence.user_id = account.user_id
  WHERE account.user_id = $1 AND account.deleted_at IS NULL AND account.is_banned = false
    AND presence.is_location_fresh = true
    AND presence.updated_at > statement_timestamp() - INTERVAL '1 hour'
    AND EXISTS (SELECT 1 FROM user_consent WHERE user_id = account.user_id
      AND consent_type = 'sensitive_data_consent' AND granted = true
      AND withdrawn_at IS NULL AND document_version = $2)
    AND EXISTS (SELECT 1 FROM user_consent WHERE user_id = account.user_id
      AND consent_type = 'location_consent' AND granted = true
      AND withdrawn_at IS NULL AND document_version = $3)
)
"#;

// Materialize exact distance once, then visit candidates in cursor order. The
// lateral LIMIT keeps eligibility checks local to each candidate. Rejected rows
// are skipped and the ordered scan continues until the page is full.
const FEED_GEOGRAPHY_SQL: &str = r#"
, located AS MATERIALIZED (
  SELECT target_presence.user_id,
    (6371.0088 * 2 * asin(sqrt(least(1.0, greatest(0.0,
      power(sin(radians((target_presence.latitude - viewer.latitude)::double precision / 2)), 2)
      + cos(radians(viewer.latitude::double precision))
      * cos(radians(target_presence.latitude::double precision))
      * power(sin(radians((target_presence.longitude - viewer.longitude)::double precision / 2)), 2)
    )))))::double precision AS distance_km,
    viewer.max_distance_km AS viewer_max_distance_km
  FROM viewer
"#;

// The spatial predicate is a conservative prefilter. The original Haversine
// formula, exact radius check and distance/UUID cursor decide the returned page.
// The meter margin covers sphere-radius and floating-point differences.
const SPATIAL_FILTER_SQL: &str = r#"
  AND target_presence.latitude BETWEEN -90 AND 90
  AND target_presence.longitude BETWEEN -180 AND 180
  AND public.st_dwithin(
    public.st_setsrid(public.st_makepoint(target_presence.longitude::double precision, target_presence.latitude::double precision), 4326)::public.geography,
    viewer.location,
    least($8::double precision, viewer.max_distance_km) * 1000.0 * 1.000001 + 1.0,
    false
  )
"#;

const FEED_RESULT_SQL_PREFIX: &str = r#"
SELECT candidate.*, metadata.viewer_ready, metadata.radius_exhausted
FROM (
  SELECT EXISTS (SELECT 1 FROM viewer) AS viewer_ready,
    COALESCE((SELECT location IS NULL OR $8::double precision >= max_distance_km FROM viewer), true) AS radius_exhausted
) AS metadata
LEFT JOIN LATERAL (
"#;

const FEED_RESULT_SQL_SUFFIX: &str = r#"
) AS candidate ON true
ORDER BY candidate.distance_km, candidate.user_id
"#;

const FEED_PAGE_SQL: &str = r#"
), nearby AS MATERIALIZED (
  SELECT user_id, distance_km FROM located
  WHERE distance_km <= least(viewer_max_distance_km, $8::double precision)
    AND ($4::double precision IS NULL OR (distance_km, user_id) > ($4::double precision, $5::uuid))
  ORDER BY distance_km, user_id
), excluded AS MATERIALIZED (
  SELECT user2_id AS user_id FROM match_init WHERE user1_id = $1
  UNION ALL SELECT user1_id FROM match_init WHERE user2_id = $1
  UNION ALL SELECT blocked_id FROM user_block WHERE blocker_id = $1
  UNION ALL SELECT blocker_id FROM user_block WHERE blocked_id = $1
  UNION ALL SELECT target_id FROM swipe_decision
    WHERE actor_id = $1 AND expires_at > statement_timestamp()
), page AS MATERIALIZED (
  SELECT target_presence.user_id, candidate.firstname, candidate.age, candidate.sex,
    candidate.unmoderated_bio, target_presence.distance_km
  FROM nearby AS target_presence
  CROSS JOIN LATERAL (
    SELECT target.firstname,
      date_part('year', age(current_date, target.birthdate))::integer AS age,
      target.sex, target.bio AS unmoderated_bio
    FROM viewer
    JOIN user_profile AS target ON target.user_id = target_presence.user_id
      AND target.sex IS NOT NULL
      AND target.birthdate > (current_date - make_interval(years => viewer.max_age + 1))::date
      AND target.birthdate <= (current_date - make_interval(years => viewer.min_age))::date
      AND ($7::uuid IS NULL OR target.user_id = $7)
    JOIN user_account AS account ON account.user_id = target.user_id
      AND account.deleted_at IS NULL AND NOT account.is_banned
    JOIN user_preferences AS preferences ON preferences.user_id = target.user_id
    WHERE viewer.age BETWEEN preferences.min_age AND preferences.max_age
      AND (viewer.looking_for = target.sex OR (viewer.looking_for = 'both' AND target.sex IN ('male', 'female')))
      AND (preferences.looking_for = viewer.sex
        OR (preferences.looking_for = 'both' AND viewer.sex IN ('male', 'female')))
      AND target_presence.distance_km <= preferences.max_distance_km
      AND EXISTS (SELECT 1 FROM user_consent WHERE user_id = target.user_id
        AND consent_type = 'sensitive_data_consent' AND granted
        AND withdrawn_at IS NULL AND document_version = $2)
      AND EXISTS (SELECT 1 FROM user_consent WHERE user_id = target.user_id
        AND consent_type = 'location_consent' AND granted
        AND withdrawn_at IS NULL AND document_version = $3)
      AND NOT EXISTS (SELECT 1 FROM excluded WHERE user_id = target.user_id)
    LIMIT 1
  ) AS candidate
  ORDER BY target_presence.distance_km, target_presence.user_id
  LIMIT $6
)
"#;

const TARGET_SETS_SQL: &str = r#"
, consented AS MATERIALIZED (
  SELECT sensitive.user_id FROM user_consent sensitive
  JOIN user_consent location ON location.user_id = sensitive.user_id
  WHERE sensitive.user_id = $7
    AND sensitive.consent_type = 'sensitive_data_consent' AND sensitive.granted
    AND sensitive.withdrawn_at IS NULL AND sensitive.document_version = $2
    AND location.consent_type = 'location_consent' AND location.granted
    AND location.withdrawn_at IS NULL AND location.document_version = $3
), excluded AS MATERIALIZED (
  SELECT $7::uuid AS user_id FROM match_init
    WHERE user1_id = least($1::uuid, $7::uuid) AND user2_id = greatest($1::uuid, $7::uuid)
  UNION ALL SELECT blocked_id FROM user_block WHERE blocker_id = $1 AND blocked_id = $7
  UNION ALL SELECT blocker_id FROM user_block WHERE blocker_id = $7 AND blocked_id = $1
)
"#;

const CANDIDATE_COLUMNS_SQL: &str = r#"
, eligible AS (
  SELECT target.user_id, target.firstname,
    date_part('year', age(current_date, target.birthdate))::integer AS age,
    target.sex, target.bio AS unmoderated_bio,
    (6371.0088 * 2 * asin(sqrt(least(1.0, greatest(0.0,
      power(sin(radians((target_presence.latitude - viewer.latitude)::double precision / 2)), 2)
      + cos(radians(viewer.latitude::double precision))
      * cos(radians(target_presence.latitude::double precision))
      * power(sin(radians((target_presence.longitude - viewer.longitude)::double precision / 2)), 2)
    )))))::double precision AS distance_km,
    viewer.max_distance_km AS viewer_max_distance_km,
    target_preferences.max_distance_km AS target_max_distance_km
  FROM viewer
"#;

const GEO_JOIN_SQL: &str = r#"
JOIN user_presence AS target_presence
    ON target_presence.user_id <> $1
    AND target_presence.is_location_fresh = true
    AND target_presence.updated_at > statement_timestamp() - INTERVAL '1 hour'
    AND target_presence.latitude BETWEEN viewer.latitude - viewer.latitude_delta
      AND viewer.latitude + viewer.latitude_delta
    AND (abs(target_presence.longitude - viewer.longitude) <= viewer.longitude_delta
      OR abs(target_presence.longitude - viewer.longitude) >= 360.0 - viewer.longitude_delta)
"#;

const CANDIDATE_PAGE_SQL: &str = r#"
JOIN user_profile AS target ON target.user_id = target_presence.user_id
    AND target.sex IS NOT NULL
    AND target.birthdate > (current_date - make_interval(years => viewer.max_age + 1))::date
    AND target.birthdate <= (current_date - make_interval(years => viewer.min_age))::date
    AND ($7::uuid IS NULL OR target.user_id = $7)
  JOIN user_account AS target_account ON target_account.user_id = target.user_id
    AND target_account.deleted_at IS NULL AND target_account.is_banned = false
  JOIN user_preferences AS target_preferences ON target_preferences.user_id = target.user_id
  WHERE viewer.age BETWEEN target_preferences.min_age AND target_preferences.max_age
    AND (viewer.looking_for = target.sex OR (viewer.looking_for = 'both' AND target.sex IN ('male', 'female')))
    AND (target_preferences.looking_for = viewer.sex
      OR (target_preferences.looking_for = 'both' AND viewer.sex IN ('male', 'female')))
    AND EXISTS (SELECT 1 FROM consented WHERE consented.user_id=target.user_id)
    AND NOT EXISTS (SELECT 1 FROM excluded WHERE excluded.user_id = target.user_id)
), page AS MATERIALIZED (
  SELECT user_id, firstname, age, sex, unmoderated_bio, distance_km
  FROM eligible
  WHERE distance_km <= least(viewer_max_distance_km, target_max_distance_km)
    AND ($4::double precision IS NULL OR (distance_km, user_id) > ($4::double precision, $5::uuid))
  ORDER BY distance_km, user_id
  LIMIT $6
)
"#;

const PAGE_CONTENT_SQL: &str = r#"
SELECT page.user_id, page.firstname, page.age, page.sex,
  CASE WHEN bio_moderation.status = 'approved' THEN page.unmoderated_bio ELSE NULL END AS bio,
  page.distance_km,
  COALESCE(traits.names, ARRAY[]::text[]) AS traits,
  COALESCE(answers.items, '[]'::jsonb) AS profile_answers
FROM page
LEFT JOIN content_moderation_case AS bio_moderation ON bio_moderation.bio_user_id = page.user_id
LEFT JOIN LATERAL (
  SELECT array_agg(trait.name ORDER BY trait.name) AS names
  FROM user_trait JOIN trait ON trait.id = user_trait.trait_id
  WHERE user_trait.user_id = page.user_id
) AS traits ON true
LEFT JOIN LATERAL (
  SELECT jsonb_agg(jsonb_build_object(
    'question_id', answer.question_id, 'code', question.code, 'question', question.prompt,
    'answer', answer.answer, 'position', answer.position
  ) ORDER BY answer.position) AS items
  FROM user_profile_answer AS answer
  JOIN profile_question AS question ON question.id = answer.question_id
  JOIN content_moderation_case AS moderation
    ON moderation.profile_answer_id = answer.id AND moderation.status = 'approved'
  WHERE answer.user_id = page.user_id
) AS answers ON true
ORDER BY page.distance_km, page.user_id
"#;
