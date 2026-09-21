use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use uuid::Uuid;

use crate::config::SecretString;
use crate::infra::crypto::encrypt_phone;
use crate::infra::postgres::{ConstraintKind, Database, DatabaseError, map_sqlx_error};

use super::otp::{OtpError, OtpService};
use super::service::{MobileAuthError, MobileAuthService, TokenPair};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MobileAccount {
    pub user_id: Uuid,
    pub is_banned: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewMobileAccount {
    pub user_id: Uuid,
    pub phone_hash: String,
    pub encrypted_phone: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountStoreError {
    Tombstone,
    Database(DatabaseError),
}

impl fmt::Display for AccountStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Tombstone => "account_tombstone",
            Self::Database(error) => return error.fmt(formatter),
        })
    }
}

impl std::error::Error for AccountStoreError {}

impl From<DatabaseError> for AccountStoreError {
    fn from(error: DatabaseError) -> Self {
        Self::Database(error)
    }
}

pub type AccountStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, AccountStoreError>> + Send + 'a>>;

pub trait MobileAccountStore: Send + Sync {
    fn find_by_phone_hash(
        &self,
        phone_hash: String,
    ) -> AccountStoreFuture<'_, Option<MobileAccount>>;
    fn create(&self, account: NewMobileAccount) -> AccountStoreFuture<'_, MobileAccount>;
}

#[derive(Clone)]
pub struct MobileAccountRepository {
    database: Database,
}

impl MobileAccountRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    async fn find_impl(
        &self,
        phone_hash: String,
    ) -> Result<Option<MobileAccount>, AccountStoreError> {
        sqlx::query_as::<_, (Uuid, bool)>(
            "SELECT user_id, is_banned FROM user_account
             WHERE phone_number_hash = $1 AND deleted_at IS NULL",
        )
        .bind(phone_hash)
        .fetch_optional(self.database.pool())
        .await
        .map(|row| row.map(|(user_id, is_banned)| MobileAccount { user_id, is_banned }))
        .map_err(|error| AccountStoreError::Database(map_sqlx_error(error)))
    }

    async fn create_impl(
        &self,
        account: NewMobileAccount,
    ) -> Result<MobileAccount, AccountStoreError> {
        self.database
            .transaction(|connection| {
                Box::pin(async move {
                    let blocked = sqlx::query_scalar::<_, bool>(
                        "SELECT EXISTS (
                           SELECT 1 FROM account_tombstone
                           WHERE phone_number_hash = $1 AND expires_at > clock_timestamp()
                         )",
                    )
                    .bind(&account.phone_hash)
                    .fetch_one(&mut *connection)
                    .await
                    .map_err(|error| AccountStoreError::Database(map_sqlx_error(error)))?;
                    if blocked {
                        return Err(AccountStoreError::Tombstone);
                    }
                    sqlx::query(
                        "INSERT INTO user_account
                         (user_id, role, phone_number_hash, phone_number_encrypted)
                         VALUES ($1, 'user', $2, $3)",
                    )
                    .bind(account.user_id)
                    .bind(account.phone_hash)
                    .bind(account.encrypted_phone)
                    .execute(&mut *connection)
                    .await
                    .map_err(|error| AccountStoreError::Database(map_sqlx_error(error)))?;
                    Ok(MobileAccount {
                        user_id: account.user_id,
                        is_banned: false,
                    })
                })
            })
            .await
    }
}

impl MobileAccountStore for MobileAccountRepository {
    fn find_by_phone_hash(
        &self,
        phone_hash: String,
    ) -> AccountStoreFuture<'_, Option<MobileAccount>> {
        Box::pin(self.find_impl(phone_hash))
    }

    fn create(&self, account: NewMobileAccount) -> AccountStoreFuture<'_, MobileAccount> {
        Box::pin(self.create_impl(account))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MobileLoginError {
    Otp(OtpError),
    AccountUnavailable,
    AccountCreationConflict,
    Internal,
}

impl fmt::Display for MobileLoginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Otp(error) => return error.fmt(formatter),
            Self::AccountUnavailable => "account_unavailable",
            Self::AccountCreationConflict => "account_creation_conflict",
            Self::Internal => "internal_error",
        })
    }
}

impl std::error::Error for MobileLoginError {}

impl From<OtpError> for MobileLoginError {
    fn from(error: OtpError) -> Self {
        Self::Otp(error)
    }
}

#[derive(Clone)]
pub struct MobileLoginService {
    otp: OtpService,
    accounts: Arc<dyn MobileAccountStore>,
    authentication: MobileAuthService,
    encryption_key: SecretString,
}

impl MobileLoginService {
    pub fn new(
        otp: OtpService,
        accounts: Arc<dyn MobileAccountStore>,
        authentication: MobileAuthService,
        encryption_key: SecretString,
    ) -> Self {
        Self {
            otp,
            accounts,
            authentication,
            encryption_key,
        }
    }

    pub async fn verify(&self, phone: &str, code: &str) -> Result<TokenPair, MobileLoginError> {
        let verified = self.otp.consume(phone, code).await?;
        let account = match self
            .accounts
            .find_by_phone_hash(verified.phone_hash.clone())
            .await
            .map_err(|_| MobileLoginError::Internal)?
        {
            Some(account) => account,
            None => {
                let encrypted_phone = encrypt_phone(&verified.phone, &self.encryption_key)
                    .map_err(|_| MobileLoginError::Internal)?;
                self.accounts
                    .create(NewMobileAccount {
                        user_id: Uuid::new_v4(),
                        phone_hash: verified.phone_hash,
                        encrypted_phone,
                    })
                    .await
                    .map_err(map_account_error)?
            }
        };
        if account.is_banned {
            return Err(MobileLoginError::AccountUnavailable);
        }
        self.authentication
            .issue_token_pair(account.user_id)
            .await
            .map_err(|error| match error {
                MobileAuthError::AccountUnavailable => MobileLoginError::AccountUnavailable,
                _ => MobileLoginError::Internal,
            })
    }
}

fn map_account_error(error: AccountStoreError) -> MobileLoginError {
    match error {
        AccountStoreError::Tombstone => MobileLoginError::AccountUnavailable,
        AccountStoreError::Database(DatabaseError::Constraint(ConstraintKind::Unique)) => {
            MobileLoginError::AccountCreationConflict
        }
        AccountStoreError::Database(_) => MobileLoginError::Internal,
    }
}
