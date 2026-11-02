//! The MCP endpoint (D31): authentication, both protocol eras, and the tools.

mod common;

use common::*;
use lazy_protocol::docs::doc_paths;
use lazy_protocol::{CreateTokenResponse, TokenScope};
use reqwest::Method;
use serde_json::{Value, json};
use sqlx::PgPool;

struct World {
    app: App,
    owner: Account,
    alice: Account,
}

async fn world(pool: PgPool) -> World {
    let app = App::start(pool).await;
    let owner = app.register("owner").await;
    let alice = app.register("alice").await;
    World { app, owner, alice }
}

async fn finish(w: World) {
    w.app.server.shutdown().await.unwrap();
}

impl World {
    async fn token(&self, who: &Account, scopes: &[TokenScope]) -> String {
        let response = self
            .app
            .call(
                Method::POST,
                doc_paths::TOKENS_ROUTE,
                who,
                Some(json!({"password": PASSWORD, "name": "mcp", "scopes": scopes})),
            )
            .await;
        assert_eq!(response.status(), 200);
        let made: CreateTokenResponse = response.json().await.unwrap();
        made.secret.expose().to_owned()
    }

    /// Everything a person's token can do through MCP.
    async fn full_token(&self, who: &Account) -> String {
        self.token(who, &TokenScope::ALL).await
    }

    /// POSTs a body to /mcp with extra headers and returns the status, headers, and body.
    async fn raw(
        &self,
        secret: Option<&str>,
        headers: &[(&str, &str)],
        body: &str,
    ) -> (u16, reqwest::header::HeaderMap, String) {
        let mut request = self
            .app
            .http
            .post(self.app.url("/mcp"))
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(body.to_owned());
        if let Some(secret) = secret {
            request = request.bearer_auth(secret);
        }
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        (status, headers, response.text().await.unwrap())
    }

    /// A legacy-era request (no version header, so the assumed 2025-03-26 rules).
    async fn legacy(&self, secret: &str, method: &str, params: Value) -> (u16, Value) {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let (status, _, text) = self
            .raw(
                Some(secret),
                &[("mcp-protocol-version", "2025-11-25")],
                &body.to_string(),
            )
            .await;
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// A call of a tool, legacy era; returns the tool result.
    async fn call(&self, secret: &str, tool: &str, arguments: Value) -> Value {
        let (status, body) = self
            .legacy(
                secret,
                "tools/call",
                json!({"name": tool, "arguments": arguments}),
            )
            .await;
        assert_eq!(status, 200, "{body}");
        assert!(body["error"].is_null(), "{body}");
        body["result"].clone()
    }

    /// A tool call that must succeed; returns its structured content.
    async fn ok(&self, secret: &str, tool: &str, arguments: Value) -> Value {
        let result = self.call(secret, tool, arguments).await;
        assert_eq!(result["isError"], false, "{tool}: {result}");
        // The text content is the structured content, serialized, for older clients.
        let text: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text, result["structuredContent"]);
        result["structuredContent"].clone()
    }

    /// A tool call that must fail as a tool error; returns its message.
    async fn fails(&self, secret: &str, tool: &str, arguments: Value) -> String {
        let result = self.call(secret, tool, arguments).await;
        assert_eq!(result["isError"], true, "{tool}: {result}");
        assert!(result.get("structuredContent").is_none());
        result["content"][0]["text"].as_str().unwrap().to_owned()
    }
}

// --- Authentication and the transport ------------------------------------------------------------

#[sqlx::test]
async fn only_an_access_token_with_mcp_use_gets_in(pool: PgPool) {
    let w = world(pool).await;
    let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string();

    // No credential: 401 with a challenge, in JSON-RPC's shape.
    let (status, headers, body) = w.raw(None, &[], &ping).await;
    assert_eq!(status, 401);
    assert!(
        headers["www-authenticate"]
            .to_str()
            .unwrap()
            .starts_with("Bearer")
    );
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["jsonrpc"], "2.0");
    assert!(body["id"].is_null() && body["error"]["code"].is_i64());

    // A token that does not exist, one that is the wrong shape.
    for bad in ["lzp_".to_owned(), format!("lzp_{}", "a".repeat(43))] {
        assert_eq!(w.raw(Some(&bad), &[], &ping).await.0, 401);
    }
    // A session is not a token.
    assert_eq!(w.raw(Some(&w.alice.token), &[], &ping).await.0, 403);
    // A token without mcp:use.
    let pages_only = w.token(&w.alice, &[TokenScope::PagesRead]).await;
    let (status, _, body) = w.raw(Some(&pages_only), &[], &ping).await;
    assert_eq!(status, 403);
    assert!(body.contains("mcp:use"));
    // With it, ping works.
    let secret = w.token(&w.alice, &[TokenScope::McpUse]).await;
    let (status, _, body) = w.raw(Some(&secret), &[], &ping).await;
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["result"],
        json!({})
    );

    // Revoking the token ends it.
    let list: lazy_protocol::TokenListResponse = w
        .app
        .call(Method::GET, doc_paths::TOKENS_ROUTE, &w.alice, None)
        .await
        .json()
        .await
        .unwrap();
    for token in list.tokens {
        w.app
            .call(Method::DELETE, &doc_paths::token(token.id), &w.alice, None)
            .await;
    }
    assert_eq!(w.raw(Some(&secret), &[], &ping).await.0, 401);
    finish(w).await;
}

#[sqlx::test]
async fn the_origin_and_the_methods_are_checked(pool: PgPool) {
    let w = world(pool).await;
    let secret = w.full_token(&w.alice).await;
    let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string();

    // A foreign Origin is refused even with a good token; the server's own is fine.
    let (status, _, _) = w
        .raw(
            Some(&secret),
            &[("origin", "https://evil.example.com")],
            &ping,
        )
        .await;
    assert_eq!(status, 403);
    let own = format!(
        "http://{}",
        w.app.server.base_url.trim_start_matches("http://")
    );
    assert_eq!(
        w.raw(Some(&secret), &[("origin", &own)], &ping).await.0,
        200
    );

    // No event stream and no sessions: GET and DELETE are 405 and say what is allowed.
    for method in [Method::GET, Method::DELETE] {
        let response = w
            .app
            .http
            .request(method, w.app.url("/mcp"))
            .bearer_auth(&secret)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 405);
        assert_eq!(response.headers()["allow"], "POST");
    }
    finish(w).await;
}

#[sqlx::test]
async fn bodies_that_are_not_one_request_are_refused(pool: PgPool) {
    let w = world(pool).await;
    let secret = w.full_token(&w.alice).await;
    let version = [("mcp-protocol-version", "2025-11-25")];

    let (status, _, body) = w.raw(Some(&secret), &version, "{not json").await;
    assert_eq!(status, 400);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["error"]["code"],
        -32700
    );

    // A batch is not accepted (removed from the protocol in 2025-06-18).
    let batch = json!([{"jsonrpc": "2.0", "id": 1, "method": "ping"}]).to_string();
    let (status, _, body) = w.raw(Some(&secret), &version, &batch).await;
    assert_eq!(status, 400);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["error"]["code"],
        -32600
    );

    let wrong = json!({"jsonrpc": "1.0", "id": 1, "method": "ping"}).to_string();
    assert_eq!(w.raw(Some(&secret), &version, &wrong).await.0, 400);
    let no_method = json!({"jsonrpc": "2.0", "id": 1}).to_string();
    assert_eq!(w.raw(Some(&secret), &version, &no_method).await.0, 400);

    // A version the server does not speak: 400 with the ones it does.
    let ping = json!({"jsonrpc": "2.0", "id": 7, "method": "ping"}).to_string();
    let (status, _, body) = w
        .raw(
            Some(&secret),
            &[("mcp-protocol-version", "1900-01-01")],
            &ping,
        )
        .await;
    assert_eq!(status, 400);
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["id"], 7);
    assert_eq!(body["error"]["code"], -32022);
    assert_eq!(body["error"]["data"]["requested"], "1900-01-01");
    assert_eq!(body["error"]["data"]["supported"][0], "2026-07-28");
    finish(w).await;
}

// --- The two protocol eras --------------------------------------------------------------------------

#[sqlx::test]
async fn the_older_revisions_open_with_initialize(pool: PgPool) {
    let w = world(pool).await;
    let secret = w.full_token(&w.alice).await;

    for (asked, answered) in [
        ("2025-11-25", "2025-11-25"),
        ("2025-06-18", "2025-06-18"),
        ("2025-03-26", "2025-03-26"),
        // One the server does not have gets the newest it does.
        ("2024-11-05", "2025-11-25"),
    ] {
        // initialize may come without a version header; that is the assumed 2025-03-26.
        let body = json!({"jsonrpc": "2.0", "id": "i", "method": "initialize", "params": {
            "protocolVersion": asked, "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}
        }});
        let (status, headers, text) = w.raw(Some(&secret), &[], &body.to_string()).await;
        assert_eq!(status, 200);
        assert!(
            headers.get("mcp-session-id").is_none(),
            "no sessions are issued"
        );
        let body: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["id"], "i");
        assert_eq!(body["result"]["protocolVersion"], answered);
        assert_eq!(
            body["result"]["capabilities"]["tools"]["listChanged"],
            false
        );
        assert_eq!(body["result"]["serverInfo"]["name"], "lazy");
        assert!(body["result"]["instructions"].is_string());
    }

    // The notification that follows is accepted with no body.
    let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string();
    let (status, _, text) = w
        .raw(
            Some(&secret),
            &[("mcp-protocol-version", "2025-11-25")],
            &note,
        )
        .await;
    assert_eq!((status, text.as_str()), (202, ""));
    // So is a response from the client (this server never asks for one).
    let response = json!({"jsonrpc": "2.0", "id": 5, "result": {}}).to_string();
    assert_eq!(w.raw(Some(&secret), &[], &response).await.0, 202);

    // An unknown method is a JSON-RPC error in a 200, as these revisions have it.
    let (status, body) = w.legacy(&secret, "resources/list", json!({})).await;
    assert_eq!(status, 200);
    assert_eq!(body["error"]["code"], -32601);
    // initialize is not a method of the newest revision, and discover is not of the older ones.
    let (status, body) = w.legacy(&secret, "server/discover", json!({})).await;
    assert_eq!(
        (status, body["error"]["code"].clone()),
        (200, json!(-32601))
    );
    finish(w).await;
}

fn modern_meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "t", "version": "1"},
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

impl World {
    async fn modern(
        &self,
        secret: &str,
        method: &str,
        mut params: Value,
        headers: &[(&str, &str)],
    ) -> (u16, Value) {
        params["_meta"] = modern_meta();
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let mut all = vec![("mcp-protocol-version", "2026-07-28")];
        all.extend_from_slice(headers);
        let (status, _, text) = self.raw(Some(secret), &all, &body.to_string()).await;
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }
}

#[sqlx::test]
async fn the_newest_revision_is_stateless_and_checks_its_headers(pool: PgPool) {
    let w = world(pool).await;
    let secret = w.full_token(&w.alice).await;

    // Discovery.
    let (status, body) = w
        .modern(
            &secret,
            "server/discover",
            json!({}),
            &[("mcp-method", "server/discover")],
        )
        .await;
    assert_eq!(status, 200);
    let result = &body["result"];
    assert_eq!(result["resultType"], "complete");
    assert_eq!(result["supportedVersions"][0], "2026-07-28");
    assert!(result["capabilities"]["tools"].is_object());
    assert_eq!(
        result["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "lazy"
    );

    // Listing and calling work with the headers; results say they are complete.
    let (status, body) = w
        .modern(
            &secret,
            "tools/list",
            json!({}),
            &[("mcp-method", "tools/list")],
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["result"]["resultType"], "complete");
    assert_eq!(body["result"]["tools"].as_array().unwrap().len(), 11);
    let (status, body) = w
        .modern(
            &secret,
            "tools/call",
            json!({"name": "list_conversations", "arguments": {}}),
            &[
                ("mcp-method", "tools/call"),
                ("mcp-name", "list_conversations"),
            ],
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["result"]["isError"], false);
    assert_eq!(body["result"]["resultType"], "complete");
    // The name may be sent in the base64 form.
    let (status, _) = w
        .modern(
            &secret,
            "tools/call",
            json!({"name": "list_conversations", "arguments": {}}),
            &[
                ("mcp-method", "tools/call"),
                ("mcp-name", "=?base64?bGlzdF9jb252ZXJzYXRpb25z?="),
            ],
        )
        .await;
    assert_eq!(status, 200);

    // Headers that are missing or disagree with the body are refused with -32020.
    for headers in [
        vec![],
        vec![("mcp-method", "ping")],
        vec![("mcp-method", "tools/call")],
        vec![("mcp-method", "tools/call"), ("mcp-name", "search")],
    ] {
        let (status, body) = w
            .modern(
                &secret,
                "tools/call",
                json!({"name": "list_conversations", "arguments": {}}),
                &headers,
            )
            .await;
        assert_eq!(
            (status, body["error"]["code"].clone()),
            (400, json!(-32020)),
            "{headers:?}"
        );
    }
    // A body whose own version disagrees with the header.
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "ping", "params": {"_meta": {
        "io.modelcontextprotocol/protocolVersion": "2025-11-25"}}});
    let (status, _, text) = w
        .raw(
            Some(&secret),
            &[
                ("mcp-protocol-version", "2026-07-28"),
                ("mcp-method", "ping"),
            ],
            &body.to_string(),
        )
        .await;
    assert_eq!(status, 400);
    assert_eq!(
        serde_json::from_str::<Value>(&text).unwrap()["error"]["code"],
        -32020
    );

    // An unknown method is a 404 with -32601, and initialize is one.
    for method in ["initialize", "resources/list"] {
        let (status, body) = w
            .modern(&secret, method, json!({}), &[("mcp-method", method)])
            .await;
        assert_eq!(
            (status, body["error"]["code"].clone()),
            (404, json!(-32601)),
            "{method}"
        );
    }
    // A client response is not allowed in this revision.
    let response = json!({"jsonrpc": "2.0", "id": 5, "result": {}}).to_string();
    assert_eq!(
        w.raw(
            Some(&secret),
            &[("mcp-protocol-version", "2026-07-28")],
            &response
        )
        .await
        .0,
        400
    );
    finish(w).await;
}

// --- Tools ------------------------------------------------------------------------------------------------

#[sqlx::test]
async fn tools_list_shows_only_what_the_scopes_allow(pool: PgPool) {
    let w = world(pool).await;
    let names = |body: &Value| -> Vec<String> {
        body["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect()
    };
    let full = w.full_token(&w.alice).await;
    let (_, all) = w.legacy(&full, "tools/list", json!({})).await;
    assert_eq!(
        names(&all),
        [
            "search",
            "fetch",
            "create_page",
            "update_page",
            "trash",
            "list_conversations",
            "list_notations",
            "add_notation",
            "complete_notation",
            "list_versions",
            "restore_version"
        ]
    );
    for tool in all["result"]["tools"].as_array().unwrap() {
        assert_eq!(tool["inputSchema"]["type"], "object");
        assert!(tool["description"].as_str().unwrap().len() > 20);
    }

    let reader = w
        .token(&w.alice, &[TokenScope::McpUse, TokenScope::PagesRead])
        .await;
    let (_, some) = w.legacy(&reader, "tools/list", json!({})).await;
    assert_eq!(names(&some), ["search", "fetch", "list_versions"]);

    let bare = w.token(&w.alice, &[TokenScope::McpUse]).await;
    let (_, none) = w.legacy(&bare, "tools/list", json!({})).await;
    assert!(none["result"]["tools"].as_array().unwrap().is_empty());

    // Calling one that is hidden is refused, naming the scope; nothing happened.
    let message = w
        .fails(
            &reader,
            "create_page",
            json!({"conversation_id": w.app.general, "title": "no"}),
        )
        .await;
    assert!(message.contains("pages:write"), "{message}");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM pages")
        .fetch_one(&w.app.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    finish(w).await;
}

#[sqlx::test]
async fn a_page_goes_through_its_whole_life_by_tool(pool: PgPool) {
    let w = world(pool).await;
    let secret = w.full_token(&w.alice).await;

    let conversations = w.ok(&secret, "list_conversations", json!({})).await;
    assert!(
        conversations["conversations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == json!(w.app.general))
    );

    let created = w
        .ok(
            &secret,
            "create_page",
            json!({"conversation_id": w.app.general, "title": "Roadmap", "body": "ship the docs"}),
        )
        .await;
    let page = created["page"]["id"].as_str().unwrap().to_owned();
    assert_eq!(created["page"]["revision"], 1);
    assert_eq!(created["page"]["content"]["body"], "ship the docs");

    let found = w.ok(&secret, "search", json!({"query": "roadmap"})).await;
    assert_eq!(found["pages"][0]["id"], page.as_str());
    let none = w.ok(&secret, "search", json!({"query": "zebra"})).await;
    assert!(none["pages"].as_array().unwrap().is_empty());

    // Edit: replace, append, and a stale one.
    let updated = w
        .ok(
            &secret,
            "update_page",
            json!({"page_id": page, "base_revision": 1, "body": "ship the docs, then rest"}),
        )
        .await;
    assert_eq!(updated["page"]["revision"], 2);
    let appended = w
        .ok(
            &secret,
            "update_page",
            json!({"page_id": page, "base_revision": 2, "append": "Done."}),
        )
        .await;
    assert_eq!(
        appended["page"]["content"]["body"],
        "ship the docs, then rest\n\nDone."
    );
    let stale = w
        .fails(
            &secret,
            "update_page",
            json!({"page_id": page, "base_revision": 1, "title": "Old"}),
        )
        .await;
    assert!(stale.starts_with("stale_revision"), "{stale}");
    let both = w
        .fails(
            &secret,
            "update_page",
            json!({"page_id": page, "base_revision": 3, "body": "a", "append": "b"}),
        )
        .await;
    assert!(both.starts_with("invalid_request"), "{both}");

    // History and restore.
    let versions = w
        .ok(&secret, "list_versions", json!({"page_id": page}))
        .await;
    let list = versions["versions"].as_array().unwrap();
    assert!(!list.is_empty());
    let first = list.last().unwrap()["version"].as_u64().unwrap();
    let current =
        w.ok(&secret, "fetch", json!({"page_id": page})).await["page"]["revision"].clone();
    let restored = w
        .ok(
            &secret,
            "restore_version",
            json!({"page_id": page, "version": first, "base_revision": current}),
        )
        .await;
    assert_eq!(restored["page"]["content"]["body"], "ship the docs");

    // A notation, completed.
    let note = w
        .ok(
            &secret,
            "add_notation",
            json!({"page_id": page, "quote": "docs", "from": 9, "to": 13, "body": "which docs?"}),
        )
        .await;
    let notation = note["notation"]["id"].as_str().unwrap().to_owned();
    let mine = w.ok(&secret, "list_notations", json!({})).await;
    assert_eq!(mine["notations"][0]["id"], notation.as_str());
    let on_page = w
        .ok(&secret, "list_notations", json!({"page_id": page}))
        .await;
    assert_eq!(on_page["notations"].as_array().unwrap().len(), 1);
    let done = w
        .ok(
            &secret,
            "complete_notation",
            json!({"notation_id": notation}),
        )
        .await;
    assert!(done["notation"]["completed_at"].is_string());
    let open = w.ok(&secret, "list_notations", json!({})).await;
    assert!(open["notations"].as_array().unwrap().is_empty());
    let completed = w
        .ok(&secret, "list_notations", json!({"completed": true}))
        .await;
    assert_eq!(completed["notations"].as_array().unwrap().len(), 1);
    let reopened = w
        .ok(
            &secret,
            "complete_notation",
            json!({"notation_id": notation, "completed": false}),
        )
        .await;
    assert!(reopened["notation"]["completed_at"].is_null());

    // Trash: it leaves the search and can be fetched no more as live content... it is still
    // readable by id as trashed, and the page row says so.
    let trashed = w.ok(&secret, "trash", json!({"page_id": page})).await;
    assert!(trashed["page"]["trashed_at"].is_string());
    let found = w.ok(&secret, "search", json!({"query": "roadmap"})).await;
    assert!(found["pages"].as_array().unwrap().is_empty());
    finish(w).await;
}

#[sqlx::test]
async fn an_idempotency_key_makes_a_repeated_write_one_write(pool: PgPool) {
    let w = world(pool).await;
    let secret = w.full_token(&w.alice).await;
    let args = json!({"conversation_id": w.app.general, "title": "Once", "body": "x",
                      "idempotency_key": "make-it-once"});
    let a = w.ok(&secret, "create_page", args.clone()).await;
    let b = w.ok(&secret, "create_page", args.clone()).await;
    assert_eq!(a["page"]["id"], b["page"]["id"]);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM pages")
        .fetch_one(&w.app.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    // The same key with other content is a conflict, not a second page.
    let other = json!({"conversation_id": w.app.general, "title": "Twice", "body": "y",
                       "idempotency_key": "make-it-once"});
    let message = w.fails(&secret, "create_page", other).await;
    assert!(message.starts_with("conflict"), "{message}");
    // Without a key, two calls are two pages.
    let plain = json!({"conversation_id": w.app.general, "title": "Free"});
    w.ok(&secret, "create_page", plain.clone()).await;
    w.ok(&secret, "create_page", plain).await;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM pages")
        .fetch_one(&w.app.pool)
        .await
        .unwrap();
    assert_eq!(count, 3);
    finish(w).await;
}

#[sqlx::test]
async fn tools_see_what_the_owner_sees_and_report_bad_arguments_as_tool_errors(pool: PgPool) {
    let w = world(pool).await;
    let alice = w.full_token(&w.alice).await;
    let owner = w.full_token(&w.owner).await;

    // A page in a private channel that the owner is not in.
    let private = w.app.create_channel(&w.alice, "secrets", "private").await;
    let made = w
        .ok(
            &alice,
            "create_page",
            json!({"conversation_id": private.id, "title": "Hidden", "body": "needle"}),
        )
        .await;
    let page = made["page"]["id"].as_str().unwrap().to_owned();
    let message = w.fails(&owner, "fetch", json!({"page_id": page})).await;
    assert!(message.starts_with("not_found"), "{message}");
    let found = w.ok(&owner, "search", json!({"query": "needle"})).await;
    assert!(found["pages"].as_array().unwrap().is_empty());
    let message = w
        .fails(
            &owner,
            "create_page",
            json!({"conversation_id": private.id, "title": "x"}),
        )
        .await;
    assert!(message.starts_with("not_found"), "{message}");

    // Mistakes in the arguments are tool errors the model can fix, never protocol errors.
    for (tool, args) in [
        ("fetch", json!({})),
        ("fetch", json!({"page_id": "not-a-uuid"})),
        ("search", json!({"query": "x", "kind": "wiki"})),
        ("update_page", json!({"page_id": page})),
        ("list_notations", json!({"scope": "everyone"})),
        (
            "complete_notation",
            json!({"notation_id": 5, "completed": "yes"}),
        ),
    ] {
        let message = w.fails(&alice, tool, args.clone()).await;
        assert!(
            message.starts_with("invalid_request"),
            "{tool} {args}: {message}"
        );
    }

    // A tool that does not exist, or a call with no name, is a protocol error.
    let (status, body) = w
        .legacy(
            &alice,
            "tools/call",
            json!({"name": "delete_everything", "arguments": {}}),
        )
        .await;
    assert_eq!(
        (status, body["error"]["code"].clone()),
        (200, json!(-32602))
    );
    let (_, body) = w.legacy(&alice, "tools/call", json!({})).await;
    assert_eq!(body["error"]["code"], -32602);
    let (_, body) = w
        .legacy(
            &alice,
            "tools/call",
            json!({"name": "search", "arguments": [1]}),
        )
        .await;
    assert_eq!(body["error"]["code"], -32602);

    // A deactivated member's token gets what their session would: nothing.
    sqlx::query("UPDATE workspace_members SET deactivated_at = now() WHERE user_id = $1")
        .bind(w.alice.user.id.as_uuid())
        .execute(&w.app.pool)
        .await
        .unwrap();
    let message = w
        .fails(
            &alice,
            "create_page",
            json!({"conversation_id": w.app.general, "title": "x"}),
        )
        .await;
    assert!(message.starts_with("not_found"), "{message}");
    let conversations = w.ok(&alice, "list_conversations", json!({})).await;
    assert!(
        conversations["conversations"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    finish(w).await;
}
