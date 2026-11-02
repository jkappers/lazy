//! Personal access tokens (llm-docs/DECISIONS.md, D31): a credential for scripts and MCP clients
//! that is not a person's session.
//!
//! A token is `lzp_` and 43 characters of random URL-safe base64. Only its SHA-256 is stored, and
//! the secret is returned once, by [`create`]. A token has the access of its owner, narrowed to
//! its scopes, and reaches only the routes in [`crate::token_routes`]. It is made with a session
//! and the owner's password, never with another token.
//!
//! # Locks
//!
//! [`create`] takes `tokens:{user}` (advisory) so the limit of [`MAX_TOKENS_PER_USER`] live tokens
//! is checked and the row written as one step.

use lazy_domain::{AccessTokenId, Timestamp, UserId};
use lazy_protocol::docs::{MAX_TOKENS_PER_USER, TOKEN_NAME_MAX_CHARS};
use lazy_protocol::{
    AccessToken, CreateTokenRequest, CreateTokenResponse, ErrorCode, TokenListResponse, TokenScope,
};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::crypto;
use crate::error::AppError;
use crate::logging::Describe;

/// Longest a token may be set to last, in days.
pub const MAX_EXPIRY_DAYS: u32 = 3_650;

/// A token that has been looked up and is usable now.
#[derive(Debug, Clone)]
pub struct TokenAuth {
    pub token_id: AccessTokenId,
    pub user_id: UserId,
    pub scopes: Vec<TokenScope>,
}

impl TokenAuth {
    pub fn has(&self, scope: TokenScope) -> bool {
        self.scopes.contains(&scope)
    }
}

#[derive(sqlx::FromRow)]
struct TokenRow {
    id: Uuid,
    name: String,
    scopes: Vec<String>,
    created_at: OffsetDateTime,
    last_used_at: Option<OffsetDateTime>,
    expires_at: Option<OffsetDateTime>,
}

impl TokenRow {
    fn to_dto(&self) -> Result<AccessToken, AppError> {
        Ok(AccessToken {
            id: AccessTokenId::from_uuid(self.id),
            name: self.name.clone(),
            scopes: parse_scopes(&self.scopes)?,
            created_at: Timestamp::from(self.created_at),
            last_used_at: self.last_used_at.map(Timestamp::from),
            expires_at: self.expires_at.map(Timestamp::from),
        })
    }
}

fn parse_scopes(stored: &[String]) -> Result<Vec<TokenScope>, AppError> {
    stored
        .iter()
        .map(|text| {
            TokenScope::parse(text).ok_or_else(|| {
                AppError::internal(format!("stored token scope {text:?} is unknown"))
            })
        })
        .collect()
}

/// Resolves a token secret to a usable token. A secret that is of the wrong shape, unknown,
/// revoked, or expired is the same failure, and so is a token whose owner is gone.
pub async fn authenticate(pool: &PgPool, secret: &str) -> Result<TokenAuth, AppError> {
    let hash =
        crypto::access_token_hash_if_wellformed(secret).ok_or_else(AppError::unauthenticated)?;
    let row: Option<(Uuid, Uuid, Vec<String>)> = sqlx::query_as(
        "SELECT id, user_id, scopes FROM access_tokens \
         WHERE token_hash = $1 AND revoked_at IS NULL \
           AND (expires_at IS NULL OR expires_at > now())",
    )
    .bind(&hash[..])
    .fetch_optional(pool)
    .await?;
    let (id, user_id, scopes) = row.ok_or_else(AppError::unauthenticated)?;
    // At most one write a minute per token, so a busy script does not write on every request. A
    // failure to record the time never fails the request.
    if let Err(error) = sqlx::query(
        "UPDATE access_tokens SET last_used_at = now() \
         WHERE id = $1 AND (last_used_at IS NULL OR last_used_at < now() - interval '1 minute')",
    )
    .bind(id)
    .execute(pool)
    .await
    {
        tracing::warn!(error = %error.describe(), "could not record when a token was used");
    }
    Ok(TokenAuth {
        token_id: AccessTokenId::from_uuid(id),
        user_id: UserId::from_uuid(user_id),
        scopes: parse_scopes(&scopes)?,
    })
}

fn invalid_name() -> AppError {
    AppError::invalid(format!(
        "a token name is 1 to {TOKEN_NAME_MAX_CHARS} characters"
    ))
}

/// Makes a token. The caller must be signed in with a session (the route refuses tokens) and
/// repeat their password.
pub async fn create(
    state: &AppState,
    user: UserId,
    request: CreateTokenRequest,
) -> Result<CreateTokenResponse, AppError> {
    let name = request.name.trim();
    if name.is_empty()
        || name.chars().count() > TOKEN_NAME_MAX_CHARS
        || name.chars().any(char::is_control)
    {
        return Err(invalid_name());
    }
    if request.scopes.is_empty() {
        return Err(AppError::invalid("a token needs at least one scope"));
    }
    let mut scopes = request.scopes.clone();
    scopes.sort();
    if scopes.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(AppError::invalid("a scope is listed twice"));
    }
    if let Some(days) = request.expires_in_days
        && !(1..=MAX_EXPIRY_DAYS).contains(&days)
    {
        return Err(AppError::invalid(format!(
            "a token lasts 1 to {MAX_EXPIRY_DAYS} days"
        )));
    }

    // The password is checked first and in the open, so a wrong one costs the same however many
    // tokens the person has.
    let stored: Option<String> =
        sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1")
            .bind(user.as_uuid())
            .fetch_optional(&state.pool)
            .await?;
    let verified = state
        .passwords
        .verify(&request.password, stored.as_deref())
        .await?;
    if !verified {
        return Err(AppError::new(
            ErrorCode::InvalidCredentials,
            "wrong password",
        ));
    }

    let (secret, hash) = crypto::new_access_token()?;
    let id = AccessTokenId::generate();
    let scope_names: Vec<&str> = scopes.iter().map(|scope| scope.as_str()).collect();
    let expires_at = request
        .expires_in_days
        .map(|days| OffsetDateTime::now_utc() + time::Duration::days(i64::from(days)));

    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("tokens:{user}"))
        .execute(&mut *tx)
        .await?;
    let live: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM access_tokens \
         WHERE user_id = $1 AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > now())",
    )
    .bind(user.as_uuid())
    .fetch_one(&mut *tx)
    .await?;
    if live as usize >= MAX_TOKENS_PER_USER {
        return Err(AppError::new(
            ErrorCode::LimitReached,
            format!("at most {MAX_TOKENS_PER_USER} tokens; revoke one first"),
        ));
    }
    let row: TokenRow = sqlx::query_as(
        "INSERT INTO access_tokens (id, user_id, name, token_hash, scopes, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         RETURNING id, name, scopes, created_at, last_used_at, expires_at",
    )
    .bind(id.as_uuid())
    .bind(user.as_uuid())
    .bind(name)
    .bind(&hash[..])
    .bind(&scope_names)
    .bind(expires_at)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(CreateTokenResponse {
        token: row.to_dto()?,
        secret,
    })
}

/// The person's tokens that have not been revoked (an expired one is listed, so it can be seen and
/// revoked), oldest first. No secret is ever returned.
pub async fn list(pool: &PgPool, user: UserId) -> Result<TokenListResponse, AppError> {
    let rows: Vec<TokenRow> = sqlx::query_as(
        "SELECT id, name, scopes, created_at, last_used_at, expires_at FROM access_tokens \
         WHERE user_id = $1 AND revoked_at IS NULL ORDER BY created_at, id",
    )
    .bind(user.as_uuid())
    .fetch_all(pool)
    .await?;
    Ok(TokenListResponse {
        tokens: rows
            .iter()
            .map(TokenRow::to_dto)
            .collect::<Result<_, _>>()?,
    })
}

/// Revokes a token of the caller's. Revoking a revoked token is not an error; a token that is
/// someone else's, or never existed, is `not_found`.
pub async fn revoke(pool: &PgPool, user: UserId, id: AccessTokenId) -> Result<(), AppError> {
    let owned: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM access_tokens WHERE id = $1 AND user_id = $2")
            .bind(id.as_uuid())
            .bind(user.as_uuid())
            .fetch_optional(pool)
            .await?;
    if owned.is_none() {
        return Err(AppError::not_found("token not found"));
    }
    sqlx::query("UPDATE access_tokens SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL")
        .bind(id.as_uuid())
        .execute(pool)
        .await?;
    Ok(())
}
