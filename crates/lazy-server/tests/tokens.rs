//! Personal access tokens (D31): making, listing, and revoking them, and what a token may reach.

mod common;

use common::*;
use lazy_domain::{AttachmentId, OperationId, PageId};
use lazy_protocol::docs::doc_paths;
use lazy_protocol::{
    CreateTokenResponse, ErrorCode, PageResponse, ProfileResponse, TokenListResponse, TokenScope,
};
use reqwest::Method;
use serde_json::{Value, json};
use sqlx::PgPool;

/// An owner and alice, both in #general.
struct World {
    app: App,
    alice: Account,
}

async fn world(pool: PgPool) -> World {
    let app = App::start(pool).await;
    let _owner = app.register("owner").await;
    let alice = app.register("alice").await;
    World { app, alice }
}

async fn finish(world: World) {
    world.app.server.shutdown().await.unwrap();
}

impl World {
    async fn make_token_raw(
        &self,
        who: &Account,
        password: &str,
        scopes: &[TokenScope],
        expires_in_days: Option<u32>,
    ) -> reqwest::Response {
        self.app
            .call(
                Method::POST,
                doc_paths::TOKENS_ROUTE,
                who,
                Some(json!({
                    "password": password, "name": "script",
                    "scopes": scopes, "expires_in_days": expires_in_days,
                })),
            )
            .await
    }

    /// Makes a token for `who` and returns its secret and description.
    async fn make_token(&self, who: &Account, scopes: &[TokenScope]) -> CreateTokenResponse {
        let response = self.make_token_raw(who, PASSWORD, scopes, None).await;
        assert_eq!(response.status(), 200, "make a token");
        response.json().await.unwrap()
    }

    /// A request made with a token instead of a session.
    async fn with(
        &self,
        secret: &str,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> reqwest::Response {
        let mut request = self
            .app
            .http
            .request(method, self.app.url(path))
            .bearer_auth(secret);
        if let Some(body) = body {
            request = request.json(&body);
        }
        request.send().await.unwrap()
    }

    async fn page(&self, who: &Account, body: &str) -> PageId {
        let page = PageId::generate();
        let response = self
            .app
            .call(
                Method::POST,
                &doc_paths::conversation_pages(self.app.general),
                who,
                Some(json!({
                    "operation_id": OperationId::generate(), "page_id": page, "kind": "page",
                    "title": "Plan", "content": {"kind": "page", "body": body},
                })),
            )
            .await;
        assert_eq!(response.status(), 200);
        page
    }
}

async fn error_of(response: reqwest::Response) -> (u16, lazy_protocol::ApiError) {
    api_error(response).await
}

#[sqlx::test]
async fn a_token_is_made_with_a_password_and_its_secret_is_shown_once(pool: PgPool) {
    let w = world(pool).await;
    let made = w
        .make_token(&w.alice, &[TokenScope::PagesRead, TokenScope::ProfileRead])
        .await;
    let secret = made.secret.expose().to_owned();
    assert!(secret.starts_with("lzp_"));
    assert_eq!(secret.len(), 47);
    assert_eq!(made.token.name, "script");
    assert_eq!(
        made.token.scopes,
        vec![TokenScope::ProfileRead, TokenScope::PagesRead]
    );

    // The list never holds the secret, and neither does the database.
    let response = w
        .app
        .call(Method::GET, doc_paths::TOKENS_ROUTE, &w.alice, None)
        .await;
    let text = response.text().await.unwrap();
    assert!(!text.contains(&secret), "the list leaked the secret");
    let list: TokenListResponse = serde_json::from_str(&text).unwrap();
    assert_eq!(list.tokens.len(), 1);
    assert_eq!(list.tokens[0].id, made.token.id);
    let stored: Vec<u8> = sqlx::query_scalar("SELECT token_hash FROM access_tokens")
        .fetch_one(&w.app.pool)
        .await
        .unwrap();
    assert_eq!(stored.len(), 32);
    assert!(
        !stored
            .windows(8)
            .any(|window| secret.as_bytes().windows(8).any(|s| s == window))
    );

    // It authenticates as alice.
    let response = w.with(&secret, Method::GET, "/v1/me", None).await;
    assert_eq!(response.status(), 200);
    let me: ProfileResponse = response.json().await.unwrap();
    assert_eq!(me.user.id, w.alice.user.id);
    finish(w).await;
}

#[sqlx::test]
async fn making_a_token_needs_the_right_password_and_a_session(pool: PgPool) {
    let w = world(pool).await;
    let response = w
        .make_token_raw(&w.alice, "not the password", &[TokenScope::PagesRead], None)
        .await;
    let (status, error) = error_of(response).await;
    assert_error(status, &error, ErrorCode::InvalidCredentials);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM access_tokens")
        .fetch_one(&w.app.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);

    // A token cannot make tokens, list them, or revoke them, whatever it holds.
    let made = w.make_token(&w.alice, &TokenScope::ALL).await;
    let secret = made.secret.expose().to_owned();
    for (method, path, body) in [
        (
            Method::POST,
            doc_paths::TOKENS_ROUTE.to_owned(),
            Some(json!({"password": PASSWORD, "name": "x", "scopes": ["pages:read"]})),
        ),
        (Method::GET, doc_paths::TOKENS_ROUTE.to_owned(), None),
        (Method::DELETE, doc_paths::token(made.token.id), None),
    ] {
        let response = w.with(&secret, method, &path, body).await;
        let (status, error) = error_of(response).await;
        assert_error(status, &error, ErrorCode::Forbidden);
        assert_eq!(error.message, lazy_server::http::TOKEN_REFUSED);
    }
    finish(w).await;
}

#[sqlx::test]
async fn a_token_is_checked_for_name_scopes_expiry_and_how_many(pool: PgPool) {
    let w = world(pool).await;
    async fn refused(w: &World, body: Value) {
        let response = w
            .app
            .call(Method::POST, doc_paths::TOKENS_ROUTE, &w.alice, Some(body))
            .await;
        let (status, error) = error_of(response).await;
        assert_error(status, &error, ErrorCode::InvalidRequest);
    }
    let ok = |name: &str, scopes: Value, days: Value| json!({"password": PASSWORD, "name": name, "scopes": scopes, "expires_in_days": days});
    refused(&w, ok("", json!(["pages:read"]), Value::Null)).await;
    refused(&w, ok("   ", json!(["pages:read"]), Value::Null)).await;
    refused(&w, ok(&"x".repeat(65), json!(["pages:read"]), Value::Null)).await;
    refused(&w, ok("a", json!([]), Value::Null)).await;
    refused(
        &w,
        ok("a", json!(["pages:read", "pages:read"]), Value::Null),
    )
    .await;
    refused(&w, ok("a", json!(["admin:write"]), Value::Null)).await;
    refused(&w, ok("a", json!(["pages:read"]), json!(0))).await;
    refused(&w, ok("a", json!(["pages:read"]), json!(3651))).await;

    // The limit counts live tokens: 25, then no more until one is revoked.
    let mut first = None;
    for _ in 0..25 {
        let made = w.make_token(&w.alice, &[TokenScope::PagesRead]).await;
        first.get_or_insert(made.token.id);
    }
    let response = w
        .make_token_raw(&w.alice, PASSWORD, &[TokenScope::PagesRead], None)
        .await;
    let (status, error) = error_of(response).await;
    assert_error(status, &error, ErrorCode::LimitReached);
    let response = w
        .app
        .call(
            Method::DELETE,
            &doc_paths::token(first.unwrap()),
            &w.alice,
            None,
        )
        .await;
    assert_eq!(response.status(), 204);
    w.make_token(&w.alice, &[TokenScope::PagesRead]).await;
    // The limit is per person.
    let owner_session = w.app.try_login("owner", PASSWORD).await;
    assert_eq!(owner_session.status(), 200);
    finish(w).await;
}

#[sqlx::test]
async fn a_token_reaches_only_what_its_scopes_allow(pool: PgPool) {
    let w = world(pool).await;
    let page = w.page(&w.alice, "hello").await;
    let read = w.make_token(&w.alice, &[TokenScope::PagesRead]).await;
    let read = read.secret.expose().to_owned();

    // pages:read reads.
    let response = w
        .with(&read, Method::GET, &doc_paths::page(page), None)
        .await;
    assert_eq!(response.status(), 200);
    let got: PageResponse = response.json().await.unwrap();
    assert_eq!(got.page.summary.title, "Plan");

    // ... and cannot write, or read notations, or the profile, or conversations.
    for (method, path, body, missing) in [
        (
            Method::DELETE,
            doc_paths::page(page),
            Some(json!({"operation_id": OperationId::generate()})),
            "pages:write",
        ),
        (
            Method::GET,
            doc_paths::page_notations(page),
            None,
            "notations:read",
        ),
        (Method::GET, "/v1/me".to_owned(), None, "profile:read"),
        (
            Method::GET,
            lazy_protocol::api::paths::open_conversation(w.app.workspace_id),
            None,
            "conversations:read",
        ),
    ] {
        let response = w.with(&read, method.clone(), &path, body).await;
        let (status, error) = error_of(response).await;
        assert_error(status, &error, ErrorCode::Forbidden);
        assert!(
            error.message.contains(missing),
            "{method} {path}: {}",
            error.message
        );
    }
    // Nothing changed.
    let response = w
        .app
        .call(Method::GET, &doc_paths::page(page), &w.alice, None)
        .await;
    let page_now: PageResponse = response.json().await.unwrap();
    assert!(page_now.page.summary.trashed_at.is_none());

    // pages:write writes, and the conversation list works with its own scope.
    let write = w
        .make_token(
            &w.alice,
            &[TokenScope::PagesWrite, TokenScope::ConversationsRead],
        )
        .await;
    let write = write.secret.expose().to_owned();
    let response = w
        .with(
            &write,
            Method::DELETE,
            &doc_paths::page(page),
            Some(json!({"operation_id": OperationId::generate()})),
        )
        .await;
    assert_eq!(response.status(), 200);
    let response = w
        .with(
            &write,
            Method::GET,
            &lazy_protocol::api::paths::open_conversation(w.app.workspace_id),
            None,
        )
        .await;
    assert_eq!(response.status(), 200);
    let list: lazy_protocol::ConversationListResponse = response.json().await.unwrap();
    assert!(list.conversations.iter().any(|c| c.id == w.app.general));
    finish(w).await;
}

#[sqlx::test]
async fn a_token_cannot_reach_anything_a_person_does_with_a_session(pool: PgPool) {
    let w = world(pool).await;
    let all = w.make_token(&w.alice, &TokenScope::ALL).await;
    let all = all.secret.expose().to_owned();
    let general = w.app.general;
    for (method, path, body) in [
        (
            Method::POST,
            lazy_protocol::api::paths::messages(general),
            Some(json!({"operation_id": OperationId::generate(), "document": {}})),
        ),
        (
            Method::GET,
            lazy_protocol::api::paths::BOOTSTRAP.to_owned(),
            None,
        ),
        (
            Method::POST,
            lazy_protocol::api::paths::LOGOUT.to_owned(),
            None,
        ),
        (
            Method::GET,
            lazy_protocol::api::paths::REALTIME.to_owned(),
            None,
        ),
        (
            Method::PATCH,
            lazy_protocol::api::paths::ME_ROUTE.to_owned(),
            Some(json!({})),
        ),
        (
            Method::GET,
            lazy_protocol::api::paths::messages(general),
            None,
        ),
    ] {
        let response = w.with(&all, method.clone(), &path, body).await;
        let (status, error) = error_of(response).await;
        assert_error(status, &error, ErrorCode::Forbidden);
        assert_eq!(
            error.message,
            lazy_server::http::TOKEN_REFUSED,
            "{method} {path}"
        );
    }
    // The session still works after all that: the token never touched it.
    let response = w.app.bootstrap(&w.alice.token).await;
    assert_eq!(response.status(), 200);
    finish(w).await;
}

#[sqlx::test]
async fn a_revoked_expired_or_unknown_token_is_unauthenticated(pool: PgPool) {
    let w = world(pool).await;
    let made = w.make_token(&w.alice, &[TokenScope::ProfileRead]).await;
    let secret = made.secret.expose().to_owned();
    assert_eq!(
        w.with(&secret, Method::GET, "/v1/me", None).await.status(),
        200
    );

    // Expiry is enforced by the clock in the database.
    sqlx::query("UPDATE access_tokens SET expires_at = now() - interval '1 second'")
        .execute(&w.app.pool)
        .await
        .unwrap();
    let (status, error) = error_of(w.with(&secret, Method::GET, "/v1/me", None).await).await;
    assert_error(status, &error, ErrorCode::Unauthenticated);
    // An expired token is still listed, so it can be seen and removed; it is not counted live.
    let list: TokenListResponse = w
        .app
        .call(Method::GET, doc_paths::TOKENS_ROUTE, &w.alice, None)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(list.tokens.len(), 1);

    // A token that expires later works.
    let response = w
        .make_token_raw(&w.alice, PASSWORD, &[TokenScope::ProfileRead], Some(30))
        .await;
    let timed: CreateTokenResponse = response.json().await.unwrap();
    assert!(timed.token.expires_at.is_some());
    assert_eq!(
        w.with(timed.secret.expose(), Method::GET, "/v1/me", None)
            .await
            .status(),
        200
    );

    // Revoking ends it at once, and revoking again is not an error.
    let response = w
        .app
        .call(
            Method::DELETE,
            &doc_paths::token(timed.token.id),
            &w.alice,
            None,
        )
        .await;
    assert_eq!(response.status(), 204);
    let (status, error) = error_of(
        w.with(timed.secret.expose(), Method::GET, "/v1/me", None)
            .await,
    )
    .await;
    assert_error(status, &error, ErrorCode::Unauthenticated);
    let response = w
        .app
        .call(
            Method::DELETE,
            &doc_paths::token(timed.token.id),
            &w.alice,
            None,
        )
        .await;
    assert_eq!(response.status(), 204);

    // Nobody else's token can be revoked or even seen.
    let other = w.app.register("bob").await;
    let response = w
        .app
        .call(
            Method::DELETE,
            &doc_paths::token(made.token.id),
            &other,
            None,
        )
        .await;
    let (status, error) = error_of(response).await;
    assert_error(status, &error, ErrorCode::NotFound);

    // Shapes that cannot be tokens are rejected without a lookup, the same way.
    for bad in [
        "lzp_".to_owned(),
        "lzp_short".to_owned(),
        format!("lzp_{}", "A".repeat(43)),
        "garbage".to_owned(),
    ] {
        let (status, error) = error_of(w.with(&bad, Method::GET, "/v1/me", None).await).await;
        assert_error(status, &error, ErrorCode::Unauthenticated);
    }
    finish(w).await;
}

#[sqlx::test]
async fn a_token_gets_what_its_owner_would_through_a_session(pool: PgPool) {
    let w = world(pool).await;
    let secret_of = |made: CreateTokenResponse| made.secret.expose().to_owned();
    let private = w.app.create_channel(&w.alice, "secrets", "private").await;
    let page = PageId::generate();
    let created = w
        .app
        .call(
            Method::POST,
            &doc_paths::conversation_pages(private.id),
            &w.alice,
            Some(json!({
                "operation_id": OperationId::generate(), "page_id": page, "kind": "page",
                "title": "Hidden", "content": {"kind": "page", "body": "x"},
            })),
        )
        .await;
    assert_eq!(created.status(), 200);
    let bob = w.app.register("bob").await;
    let bob_token = secret_of(w.make_token(&bob, &TokenScope::ALL).await);
    let alice_token = secret_of(w.make_token(&w.alice, &TokenScope::ALL).await);

    // Bob is not in the private channel: through a token it is `not_found`, as for his session.
    for secret in [&bob_token, &bob.token] {
        let (status, error) = error_of(
            w.with(secret, Method::GET, &doc_paths::page(page), None)
                .await,
        )
        .await;
        assert_error(status, &error, ErrorCode::NotFound);
    }
    assert_eq!(
        w.with(&alice_token, Method::GET, &doc_paths::page(page), None)
            .await
            .status(),
        200
    );

    // Leaving the channel takes the access away from the token at once.
    w.app.remove_from_conversation(private.id, &w.alice).await;
    let (status, error) = error_of(
        w.with(&alice_token, Method::GET, &doc_paths::page(page), None)
            .await,
    )
    .await;
    assert_error(status, &error, ErrorCode::NotFound);

    // A deactivated member gets what a session gets: nothing of the workspace.
    let general_page = w.page(&bob, "public").await;
    assert_eq!(
        w.with(
            &bob_token,
            Method::GET,
            &doc_paths::page(general_page),
            None
        )
        .await
        .status(),
        200
    );
    sqlx::query("UPDATE workspace_members SET deactivated_at = now() WHERE user_id = $1")
        .bind(bob.user.id.as_uuid())
        .execute(&w.app.pool)
        .await
        .unwrap();
    for secret in [&bob_token, &bob.token] {
        let (status, error) = error_of(
            w.with(secret, Method::GET, &doc_paths::page(general_page), None)
                .await,
        )
        .await;
        assert_error(status, &error, ErrorCode::NotFound);
    }
    finish(w).await;
}

#[sqlx::test]
async fn last_used_is_written_at_most_once_a_minute(pool: PgPool) {
    let w = world(pool).await;
    let made = w.make_token(&w.alice, &[TokenScope::ProfileRead]).await;
    let secret = made.secret.expose().to_owned();
    let read = |pool: PgPool| async move {
        sqlx::query_scalar::<_, Option<time::OffsetDateTime>>(
            "SELECT last_used_at FROM access_tokens",
        )
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    assert!(read(w.app.pool.clone()).await.is_none());
    w.with(&secret, Method::GET, "/v1/me", None).await;
    let first = read(w.app.pool.clone()).await.expect("recorded");
    w.with(&secret, Method::GET, "/v1/me", None).await;
    assert_eq!(
        read(w.app.pool.clone()).await,
        Some(first),
        "no new write within a minute"
    );
    sqlx::query("UPDATE access_tokens SET last_used_at = now() - interval '2 minutes'")
        .execute(&w.app.pool)
        .await
        .unwrap();
    w.with(&secret, Method::GET, "/v1/me", None).await;
    let later = read(w.app.pool.clone()).await.unwrap();
    assert!(later > first - time::Duration::minutes(1));
    assert!(later > time::OffsetDateTime::now_utc() - time::Duration::seconds(30));
    finish(w).await;
}

#[sqlx::test]
async fn a_token_downloads_the_files_of_pages_and_nothing_else(pool: PgPool) {
    let w = world(pool).await;
    let page = w.page(&w.alice, "with a file").await;
    let token = w
        .make_token(&w.alice, &[TokenScope::PagesRead, TokenScope::PagesWrite])
        .await;
    let secret = token.secret.expose().to_owned();

    // Upload to the page with the token.
    let file = AttachmentId::generate();
    let response = w
        .app
        .http
        .put(w.app.url(&doc_paths::page_file(page, file)))
        .bearer_auth(&secret)
        .header("content-type", "application/octet-stream")
        .header("x-attachment-name", "notes.bin")
        .body(b"bytes".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "upload");

    // The page's file downloads with the token.
    let download = lazy_protocol::api::paths::attachment(file);
    let response = w.with(&secret, Method::GET, &download, None).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.bytes().await.unwrap().as_ref(), b"bytes");

    // A file on a message (the same route) is not found by a token, though alice's session reads it.
    let message_file = AttachmentId::generate();
    let response = w
        .app
        .http
        .put(w.app.url(&lazy_protocol::api::paths::attachment_upload(
            w.app.general,
            message_file,
        )))
        .bearer_auth(&w.alice.token)
        .header("content-type", "application/octet-stream")
        .header("x-attachment-name", "m.bin")
        .body(b"message bytes".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "message upload");
    let download = lazy_protocol::api::paths::attachment(message_file);
    let (status, error) = error_of(w.with(&secret, Method::GET, &download, None).await).await;
    assert_error(status, &error, ErrorCode::NotFound);
    assert_eq!(
        w.app
            .call(Method::GET, &download, &w.alice, None)
            .await
            .status(),
        200
    );
    finish(w).await;
}
