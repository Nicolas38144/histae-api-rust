use histae_api_rust::infra::{
    crypto::sha256_hex,
    postgres::{Database, DatabaseError, map_sqlx_error},
};
use uuid::Uuid;
pub async fn grant(database: &Database, user: Uuid, original: &str) -> Result<(), DatabaseError> {
    let mut c = database.acquire().await?;
    let account: Uuid = sqlx::query_scalar(
        "INSERT INTO billing_store_account(user_id,google_account_id)
  VALUES($1,$2) ON CONFLICT(user_id) DO UPDATE SET user_id=EXCLUDED.user_id RETURNING id",
    )
    .bind(user)
    .bind(sha256_hex(user.as_bytes()))
    .fetch_one(&mut *c)
    .await
    .map_err(map_sqlx_error)?;
    sqlx::query("INSERT INTO billing_store_subscription(user_id,account_id,provider,original_id,reference_encrypted,
  environment,product_id,transaction_id,billing_period,status,starts_at,expires_at,auto_renew,acknowledged,verified_at)
  VALUES($1,$2,'app_store',$3,''::bytea,'sandbox','premium_monthly',$3,'monthly','active',
   clock_timestamp()-interval '1 day',clock_timestamp()+interval '30 days',true,true,clock_timestamp())")
  .bind(user).bind(account).bind(original).execute(&mut*c).await.map_err(map_sqlx_error)?;
    Ok(())
}
