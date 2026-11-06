//! Bounded, fixed-origin imports. Network I/O always precedes the policy transaction.
use super::domain::{OBSERVATION_DAYS, POLICY_LOCK, PhoneKey, PolicyError};
use crate::{
    config::SecretString,
    infra::postgres::{Database, map_sqlx_error},
};
use chrono::{DateTime, NaiveDate, Utc};
use std::{collections::BTreeMap, time::Duration};
use uuid::Uuid;

pub const MAX_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 200_000;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Source {
    Castle,
    Ip1,
}
impl Source {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Castle => "castle",
            Self::Ip1 => "ip1",
        }
    }
    pub const fn url(self) -> &'static str {
        match self {
            Self::Castle => {
                "https://raw.githubusercontent.com/castle/disposable-phone-numbers/master/disposable-phone-numbers.txt"
            }
            Self::Ip1 => {
                "https://raw.githubusercontent.com/iP1SMS/disposable-phone-numbers/master/number-list.json"
            }
        }
    }
}
pub struct ImportedNumber {
    pub key: PhoneKey,
    pub last_seen_at: DateTime<Utc>,
}

pub fn parse(
    source: Source,
    body: &[u8],
    now: DateTime<Utc>,
    key: &SecretString,
) -> Result<Vec<ImportedNumber>, PolicyError> {
    if body.is_empty() || body.len() > MAX_BYTES {
        return Err(PolicyError::Invalid);
    }
    let mut values = Vec::new();
    match source {
        Source::Castle => {
            let text = std::str::from_utf8(body).map_err(|_| PolicyError::Invalid)?;
            for line in text.lines().filter(|s| !s.trim().is_empty()) {
                values.push((line.trim().to_owned(), now));
            }
        }
        Source::Ip1 => {
            let map: BTreeMap<String, String> =
                serde_json::from_slice(body).map_err(|_| PolicyError::Invalid)?;
            for (phone, date) in map {
                let date = NaiveDate::parse_from_str(&date, "%Y-%m-%d")
                    .map_err(|_| PolicyError::Invalid)?;
                let time = date
                    .and_hms_opt(0, 0, 0)
                    .ok_or(PolicyError::Invalid)?
                    .and_utc();
                if time > now + chrono::Duration::days(1) {
                    return Err(PolicyError::Invalid);
                }
                values.push((format!("+{phone}"), time.min(now)));
            }
        }
    }
    let minimum = match source {
        Source::Castle => 100,
        Source::Ip1 => 1000,
    };
    if values.len() < minimum || values.len() > MAX_ENTRIES {
        return Err(PolicyError::Invalid);
    }
    let mut valid = 0;
    let mut french = BTreeMap::new();
    for (phone, last_seen_at) in &values {
        let digits = phone.strip_prefix('+').ok_or(PolicyError::Invalid)?;
        if !(8..=15).contains(&digits.len())
            || !digits.bytes().all(|b| b.is_ascii_digit())
            || digits.starts_with('0')
        {
            continue;
        }
        valid += 1;
        if phone.starts_with("+33")
            && *last_seen_at > now - chrono::Duration::days(OBSERVATION_DAYS)
            && let Ok(key) = PhoneKey::new(phone, key)
        {
            french.insert(
                key.hash.clone(),
                ImportedNumber {
                    key,
                    last_seen_at: *last_seen_at,
                },
            );
        }
    }
    // Reject malformed/error documents, while permitting valid lists with no French numbers.
    if valid == 0 || valid * 100 < values.len() * 90 {
        return Err(PolicyError::Invalid);
    }
    Ok(french.into_values().collect())
}

pub async fn fetch(source: Source, key: &SecretString) -> Result<Vec<ImportedNumber>, PolicyError> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(5))
        .build()
        .map_err(|_| PolicyError::Unavailable)?;
    let mut response = client
        .get(source.url())
        .send()
        .await
        .map_err(|_| PolicyError::Unavailable)?;
    if response.status() != reqwest::StatusCode::OK
        || response
            .content_length()
            .is_some_and(|n| n > MAX_BYTES as u64)
    {
        return Err(PolicyError::Invalid);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| PolicyError::Unavailable)?
    {
        if chunk.len() > MAX_BYTES.saturating_sub(body.len()) {
            return Err(PolicyError::Invalid);
        }
        body.extend_from_slice(&chunk);
    }
    parse(source, &body, Utc::now(), key)
}

pub async fn import(
    database: &Database,
    source: Source,
    numbers: Vec<ImportedNumber>,
    key: &SecretString,
) -> Result<usize, PolicyError> {
    let count = numbers.len();
    if count > MAX_ENTRIES {
        return Err(PolicyError::Invalid);
    }
    let fingerprint = super::pg::key_fingerprint(key)?;
    database.transaction(|connection| { Box::pin(async move {
        sqlx::query("SELECT pg_advisory_xact_lock($1)").bind(POLICY_LOCK)
            .execute(&mut *connection).await.map_err(map_sqlx_error)?;
        super::pg::verify_key_on(connection,&fingerprint).await?;
        let ids=numbers.iter().map(|_|Uuid::new_v4()).collect::<Vec<_>>();
        let hashes=numbers.iter().map(|n|n.key.hash.clone()).collect::<Vec<_>>();
        let hints=numbers.iter().map(|n|n.key.hint.clone()).collect::<Vec<_>>();
        let dates=numbers.iter().map(|n|n.last_seen_at).collect::<Vec<_>>();
        sqlx::query("INSERT INTO phone_blocklist_entry(id,phone_hash,phone_hint)
            SELECT * FROM unnest($1::uuid[],$2::text[],$3::text[]) ON CONFLICT(phone_hash) DO NOTHING")
            .bind(ids).bind(&hashes).bind(hints).execute(&mut *connection).await.map_err(map_sqlx_error)?;
        sqlx::query("INSERT INTO phone_blocklist_observation(entry_id,source,last_seen_at)
            SELECT e.id,$1,data.seen FROM unnest($2::text[],$3::timestamptz[]) data(hash,seen)
            JOIN phone_blocklist_entry e ON e.phone_hash=data.hash
            ON CONFLICT(entry_id,source) DO UPDATE SET last_seen_at=GREATEST(phone_blocklist_observation.last_seen_at,EXCLUDED.last_seen_at),imported_at=clock_timestamp()")
            .bind(source.name()).bind(hashes).bind(dates).execute(&mut *connection).await.map_err(map_sqlx_error)?;
        sqlx::query("UPDATE phone_blocklist_source SET last_attempt_at=clock_timestamp(),last_success_at=clock_timestamp(),last_error=NULL,imported_count=$2 WHERE source=$1")
            .bind(source.name()).bind(count as i32).execute(connection).await.map_err(map_sqlx_error)?;
        Ok(count)
    }) }).await
}
pub async fn record_failure(database: &Database, source: Source) -> Result<(), PolicyError> {
    sqlx::query("UPDATE phone_blocklist_source SET last_attempt_at=clock_timestamp(),last_error='phone_feed_unavailable' WHERE source=$1")
        .bind(source.name()).execute(database.acquire().await?.as_mut()).await.map_err(map_sqlx_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key() -> SecretString {
        SecretString::new("f".repeat(32))
    }
    fn castle(extra: &str) -> Vec<u8> {
        let mut lines = (0..100)
            .map(|n| format!("+1202000{n:04}"))
            .collect::<Vec<_>>();
        lines.push(extra.to_owned());
        lines.join("\n").into_bytes()
    }
    #[test]
    fn validates_bounded_documents_and_only_keeps_canonical_french_numbers() {
        let now = Utc::now();
        let numbers = parse(Source::Castle, &castle("+33600000000"), now, &key()).unwrap();
        assert_eq!(numbers.len(), 1);
        assert_eq!(
            numbers[0].key.hash,
            PhoneKey::new("+33 6 00 00 00 00", &key()).unwrap().hash
        );
        assert!(!numbers[0].key.hint.contains("600000000"));
        for body in [
            b"".as_slice(),
            b"<html>error</html>",
            b"+33600000000",
            &vec![b'+'; MAX_BYTES + 1],
        ] {
            assert!(parse(Source::Castle, body, now, &key()).is_err());
        }
        assert!(
            parse(
                Source::Castle,
                &castle("https://localhost/private"),
                now,
                &key()
            )
            .is_err()
        );
        assert!(
            parse(Source::Castle, &castle("+337000000000"), now, &key())
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn dated_sources_never_refresh_an_old_observation_or_accept_future_dates() {
        let now = Utc::now();
        let mut map = (0..1000)
            .map(|n| (format!("1202000{n:04}"), now.date_naive().to_string()))
            .collect::<BTreeMap<_, _>>();
        let old = now - chrono::Duration::days(100);
        map.insert("33600000000".to_owned(), old.date_naive().to_string());
        let bytes = serde_json::to_vec(&map).unwrap();
        assert!(parse(Source::Ip1, &bytes, now, &key()).unwrap().is_empty());
        map.insert(
            "33600000000".to_owned(),
            (now - chrono::Duration::days(2)).date_naive().to_string(),
        );
        let numbers = parse(Source::Ip1, &serde_json::to_vec(&map).unwrap(), now, &key()).unwrap();
        assert_eq!(numbers.len(), 1);
        assert!(numbers[0].last_seen_at < now - chrono::Duration::days(1));
        map.insert(
            "33600000000".to_owned(),
            (now + chrono::Duration::days(5)).date_naive().to_string(),
        );
        assert!(parse(Source::Ip1, &serde_json::to_vec(&map).unwrap(), now, &key()).is_err());
    }
}
