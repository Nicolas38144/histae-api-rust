use super::{WebChallenge, WebIdentity};
use crate::infra::postgres::DatabaseError;
use std::{future::Future, pin::Pin};
use uuid::Uuid;

pub type WebAccountFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, DatabaseError>> + Send + 'a>>;

/// Session storage. Implementations atomically consume attempts/challenges.
pub trait WebAccountStore: Send + Sync {
    fn challenge<'a>(
        &'a self,
        key: Uuid,
        user: Uuid,
        phone_hash: &'a str,
        language: &'a str,
    ) -> WebAccountFuture<'a, Option<WebChallenge>>;
    fn begin_verify<'a>(
        &'a self,
        id: Uuid,
        phone_hash: &'a str,
    ) -> WebAccountFuture<'a, Option<Uuid>>;
    fn open_session<'a>(
        &'a self,
        challenge: Uuid,
        user: Uuid,
        token: &'a str,
    ) -> WebAccountFuture<'a, bool>;
    fn identity<'a>(&'a self, token: &'a str) -> WebAccountFuture<'a, Option<WebIdentity>>;
    fn logout<'a>(&'a self, token: &'a str) -> WebAccountFuture<'a, ()>;
}
