use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use reqwest::{Client, Method, StatusCode};
use sha2::{Digest, Sha256};
use tokio::time::{Instant, timeout_at};
use url::Url;

use super::storage::{ObjectStorageError, PhotoObjectStorage, StorageFuture};
use crate::config::ObjectStorageConfig;
use crate::http::health::{DependencyProbe, ProbeError, ProbeFuture};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_ATTEMPTS: u8 = 3;

#[derive(Clone)]
pub struct S3ObjectStorage {
    client: Client,
    endpoint: Url,
    region: String,
    bucket: String,
    access_key: String,
    secret_key: String,
    force_path_style: bool,
}

impl S3ObjectStorage {
    pub fn new(config: &ObjectStorageConfig) -> Result<Self, ObjectStorageError> {
        let client = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| ObjectStorageError)?;
        Ok(Self {
            client,
            endpoint: config.endpoint.clone(),
            region: config.region.clone(),
            bucket: config.bucket.clone(),
            access_key: config.access_key.clone(),
            secret_key: config.secret_key.expose_secret().to_owned(),
            force_path_style: config.force_path_style,
        })
    }

    fn object_url(&self, key: Option<&str>) -> Result<Url, ObjectStorageError> {
        let mut url = self.endpoint.clone();
        url.set_query(None);
        url.set_fragment(None);
        if self.force_path_style {
            let path = match key {
                Some(key) => format!("/{}/{key}", self.bucket),
                None => format!("/{}/", self.bucket),
            };
            url.set_path(&path);
        } else {
            let host = url.host_str().ok_or(ObjectStorageError)?;
            url.set_host(Some(&format!("{}.{}", self.bucket, host)))
                .map_err(|_| ObjectStorageError)?;
            url.set_path(&key.map_or_else(|| "/".to_owned(), |key| format!("/{key}")));
        }
        Ok(url)
    }

    async fn send(
        &self,
        method: Method,
        key: Option<&str>,
        body: &[u8],
        content_type: Option<&str>,
        cache_control: Option<&str>,
    ) -> Result<(), ObjectStorageError> {
        let url = self.object_url(key)?;
        let now = Utc::now();
        let payload_hash = hex(&Sha256::digest(body));
        let mut headers = BTreeMap::new();
        if let Some(value) = cache_control {
            headers.insert("cache-control", value.to_owned());
        }
        if let Some(value) = content_type {
            headers.insert("content-type", value.to_owned());
        }
        headers.insert("host", authority(&url)?);
        headers.insert("x-amz-content-sha256", payload_hash.clone());
        headers.insert("x-amz-date", now.format("%Y%m%dT%H%M%SZ").to_string());
        let authorization = self.authorization(&method, &url, &headers, &payload_hash, now)?;

        let deadline = Instant::now() + REQUEST_TIMEOUT;
        for attempt in 1..=MAX_ATTEMPTS {
            let mut request = self.client.request(method.clone(), url.clone());
            for (name, value) in &headers {
                request = request.header(*name, value);
            }
            request = request.header("authorization", &authorization);
            if !body.is_empty() {
                request = request.body(body.to_vec());
            }
            match timeout_at(deadline, request.send()).await {
                Ok(Ok(response)) if response.status().is_success() => return Ok(()),
                Ok(Ok(response)) if attempt < MAX_ATTEMPTS && retryable(response.status()) => {
                    continue;
                }
                Ok(Err(_)) if attempt < MAX_ATTEMPTS => continue,
                _ => return Err(ObjectStorageError),
            }
        }
        Err(ObjectStorageError)
    }

    fn authorization(
        &self,
        method: &Method,
        url: &Url,
        headers: &BTreeMap<&str, String>,
        payload_hash: &str,
        now: DateTime<Utc>,
    ) -> Result<String, ObjectStorageError> {
        let signed_headers = headers.keys().copied().collect::<Vec<_>>().join(";");
        let canonical_headers = headers
            .iter()
            .map(|(name, value)| format!("{name}:{}\n", value.trim()))
            .collect::<String>();
        let canonical = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            method.as_str(),
            canonical_path(url),
            url.query().unwrap_or_default(),
            canonical_headers,
            signed_headers,
            payload_hash,
        );
        let stamp = now.format("%Y%m%d").to_string();
        let scope = format!("{stamp}/{}/s3/aws4_request", self.region);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            now.format("%Y%m%dT%H%M%SZ"),
            scope,
            hex(&Sha256::digest(canonical.as_bytes())),
        );
        let signature = hex(&hmac(
            &signing_key(&self.secret_key, &stamp, &self.region)?,
            string_to_sign.as_bytes(),
        )?);
        Ok(format!(
            "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
            self.access_key, scope, signed_headers, signature,
        ))
    }

    fn presign(&self, key: &str, ttl_seconds: u32) -> Result<String, ObjectStorageError> {
        let mut url = self.object_url(Some(key))?;
        let now = Utc::now();
        let stamp = now.format("%Y%m%d").to_string();
        let scope = format!("{stamp}/{}/s3/aws4_request", self.region);
        let host = authority(&url)?;
        let mut query = BTreeMap::new();
        query.insert("X-Amz-Algorithm", "AWS4-HMAC-SHA256".to_owned());
        query.insert("X-Amz-Credential", format!("{}/{}", self.access_key, scope));
        query.insert("X-Amz-Date", now.format("%Y%m%dT%H%M%SZ").to_string());
        query.insert("X-Amz-Expires", ttl_seconds.to_string());
        query.insert("X-Amz-SignedHeaders", "host".to_owned());
        let canonical_query = query
            .iter()
            .map(|(key, value)| format!("{}={}", aws_encode(key, false), aws_encode(value, false)))
            .collect::<Vec<_>>()
            .join("&");
        let canonical = format!(
            "GET\n{}\n{}\nhost:{}\n\nhost\nUNSIGNED-PAYLOAD",
            canonical_path(&url),
            canonical_query,
            host,
        );
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            now.format("%Y%m%dT%H%M%SZ"),
            scope,
            hex(&Sha256::digest(canonical.as_bytes())),
        );
        let signature = hex(&hmac(
            &signing_key(&self.secret_key, &stamp, &self.region)?,
            string_to_sign.as_bytes(),
        )?);
        url.set_query(Some(&format!(
            "{canonical_query}&X-Amz-Signature={signature}"
        )));
        Ok(url.to_string())
    }
}

impl PhotoObjectStorage for S3ObjectStorage {
    fn put<'a>(
        &'a self,
        key: &'a str,
        body: Vec<u8>,
        content_type: &'static str,
        cache_control: &'static str,
    ) -> StorageFuture<'a, ()> {
        Box::pin(async move {
            self.send(
                Method::PUT,
                Some(key),
                &body,
                Some(content_type),
                Some(cache_control),
            )
            .await
        })
    }

    fn delete<'a>(&'a self, key: &'a str) -> StorageFuture<'a, ()> {
        Box::pin(async move { self.send(Method::DELETE, Some(key), &[], None, None).await })
    }

    fn signed_get_url<'a>(&'a self, key: &'a str, ttl_seconds: u32) -> StorageFuture<'a, String> {
        Box::pin(async move { self.presign(key, ttl_seconds) })
    }

    fn check(&self) -> StorageFuture<'_, ()> {
        Box::pin(async move { self.send(Method::HEAD, None, &[], None, None).await })
    }
}

impl DependencyProbe for S3ObjectStorage {
    fn check(&self) -> ProbeFuture<'_> {
        Box::pin(async move {
            PhotoObjectStorage::check(self)
                .await
                .map_err(|_| ProbeError)
        })
    }
}

fn retryable(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn authority(url: &Url) -> Result<String, ObjectStorageError> {
    let host = url.host_str().ok_or(ObjectStorageError)?;
    Ok(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    })
}

fn canonical_path(url: &Url) -> String {
    if url.path().is_empty() {
        "/".to_owned()
    } else {
        url.path().to_owned()
    }
}

fn aws_encode(value: &str, preserve_slash: bool) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'_' | b'.' | b'~')
            || (preserve_slash && byte == b'/')
        {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(&mut encoded, "%{byte:02X}");
        }
    }
    encoded
}

fn signing_key(secret: &str, date: &str, region: &str) -> Result<Vec<u8>, ObjectStorageError> {
    let date_key = hmac(format!("AWS4{secret}").as_bytes(), date.as_bytes())?;
    let region_key = hmac(&date_key, region.as_bytes())?;
    let service_key = hmac(&region_key, b"s3")?;
    hmac(&service_key, b"aws4_request")
}

fn hmac(key: &[u8], value: &[u8]) -> Result<Vec<u8>, ObjectStorageError> {
    let mut signer = Hmac::<Sha256>::new_from_slice(key).map_err(|_| ObjectStorageError)?;
    signer.update(value);
    Ok(signer.finalize().into_bytes().to_vec())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(&mut value, "{byte:02x}");
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_s3_paths_without_losing_object_separators() {
        assert_eq!(
            aws_encode("profile-photos/a b.webp", true),
            "profile-photos/a%20b.webp"
        );
        assert_eq!(aws_encode("a/b", false), "a%2Fb");
    }
}
