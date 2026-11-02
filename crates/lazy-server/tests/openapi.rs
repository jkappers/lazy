//! The route table and the OpenAPI description (D31): every route the server registers is either
//! reachable by a token with a named scope, public, or refuses a token; and the description
//! matches both the table and what the server really answers.

mod common;

use std::collections::{HashMap, HashSet};

use common::*;
use lazy_domain::{OperationId, PageId};
use lazy_protocol::docs::doc_paths;
use lazy_protocol::{CreateTokenResponse, TokenScope};
use lazy_server::http::TOKEN_REFUSED;
use lazy_server::token_routes::{PUBLIC_ROUTES, TOKEN_ROUTES};
use reqwest::Method;
use serde_json::{Value, json};
use sqlx::PgPool;

const METHODS: [&str; 5] = ["GET", "POST", "PUT", "PATCH", "DELETE"];

fn crate_file(relative: &str) -> String {
    let path = format!("{}/{relative}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// `pub const NAME: &str = "value";` in the protocol crate's route modules.
fn route_constants() -> HashMap<String, String> {
    let mut constants = HashMap::new();
    for file in [
        "../lazy-protocol/src/api.rs",
        "../lazy-protocol/src/docs.rs",
    ] {
        let text = crate_file(file);
        let mut rest = text.as_str();
        while let Some(at) = rest.find("pub const ") {
            rest = &rest[at + "pub const ".len()..];
            let Some((name, after)) = rest.split_once(':') else {
                break;
            };
            let name = name.trim();
            if !after.trim_start().starts_with("&str") {
                continue;
            }
            let Some(open) = after.find('"') else { break };
            let value = &after[open + 1..];
            let Some(close) = value.find('"') else { break };
            constants.insert(name.to_owned(), value[..close].to_owned());
        }
    }
    constants
}

/// Every route template the server registers, read from its router source.
fn registered_routes() -> Vec<String> {
    let constants = route_constants();
    let mut routes = Vec::new();
    for file in ["src/http.rs", "src/docs_http.rs"] {
        let text = crate_file(file);
        let mut rest = text.as_str();
        while let Some(at) = rest.find(".route(") {
            rest = &rest[at + ".route(".len()..];
            let argument = rest.trim_start();
            let end = argument.find(',').expect("route argument");
            let argument = argument[..end].trim();
            if let Some(literal) = argument.strip_prefix('"') {
                routes.push(literal.trim_end_matches('"').to_owned());
            } else {
                let name = argument.rsplit("::").next().unwrap();
                routes.push(
                    constants
                        .get(name)
                        .unwrap_or_else(|| panic!("no route constant {name}"))
                        .clone(),
                );
            }
        }
    }
    routes.sort();
    routes.dedup();
    routes
}

fn fill(template: &str) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let close = rest[open..].find('}').unwrap() + open;
        match &rest[open + 1..close] {
            "version" => out.push('1'),
            _ => out.push_str(&uuid::Uuid::new_v4().to_string()),
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

async fn token_with(app: &App, who: &Account, scopes: &[TokenScope]) -> String {
    let response = app
        .call(
            Method::POST,
            doc_paths::TOKENS_ROUTE,
            who,
            Some(json!({"password": PASSWORD, "name": "t", "scopes": scopes})),
        )
        .await;
    assert_eq!(response.status(), 200);
    let made: CreateTokenResponse = response.json().await.unwrap();
    made.secret.expose().to_owned()
}

#[test]
fn the_route_scan_finds_the_routes() {
    let routes = registered_routes();
    assert!(routes.len() > 70, "found only {}", routes.len());
    assert!(routes.iter().any(|r| r == doc_paths::PAGE_ROUTE));
    assert!(routes.iter().any(|r| r == "/mcp"));
    // Every table entry is a route the scan found, and every public route too.
    for route in TOKEN_ROUTES {
        assert!(routes.iter().any(|r| r == route.path), "{}", route.path);
    }
    for (_, path) in PUBLIC_ROUTES {
        assert!(routes.iter().any(|r| r == path), "{path}");
    }
}

#[sqlx::test]
async fn every_registered_route_is_in_the_table_public_or_refuses_tokens(pool: PgPool) {
    let app = App::start(pool).await;
    let _owner = app.register("owner").await;
    let alice = app.register("alice").await;
    let everything = token_with(&app, &alice, &TokenScope::ALL).await;
    let routes = registered_routes();
    let table: HashSet<(&str, &str)> = TOKEN_ROUTES.iter().map(|r| (r.method, r.path)).collect();
    let public: HashSet<(&str, &str)> = PUBLIC_ROUTES.iter().copied().collect();
    let mut probed = 0;

    for template in &routes {
        for method in METHODS {
            let key = (method, template.as_str());
            if public.contains(&key) {
                continue;
            }
            let request = app
                .http
                .request(
                    Method::from_bytes(method.as_bytes()).unwrap(),
                    app.url(&fill(template)),
                )
                .bearer_auth(&everything);
            let response = request.send().await.unwrap();
            let status = response.status().as_u16();
            if status == 405 {
                continue; // not a route for this method
            }
            probed += 1;
            let (_, error) = (
                status,
                response.json::<lazy_protocol::ApiError>().await.ok(),
            );
            let message = error.as_ref().map(|e| e.message.as_str()).unwrap_or("");
            if table.contains(&key) {
                assert!(
                    message != TOKEN_REFUSED,
                    "{method} {template} is in the table but refuses a token holding every scope"
                );
                assert!(
                    message != "no such route",
                    "{method} {template} is in the table but is not a route"
                );
            } else {
                assert_eq!(
                    (status, message),
                    (403, TOKEN_REFUSED),
                    "{method} {template} is neither in the table nor public, so it must refuse a token"
                );
            }
        }
    }
    assert!(probed > 80, "probed only {probed}");

    // Every table entry is a route the server really has.
    for route in TOKEN_ROUTES {
        let response = app
            .http
            .request(
                Method::from_bytes(route.method.as_bytes()).unwrap(),
                app.url(&fill(route.path)),
            )
            .bearer_auth(&everything)
            .send()
            .await
            .unwrap();
        assert_ne!(
            response.status().as_u16(),
            405,
            "{} {}",
            route.method,
            route.path
        );
    }

    // Public routes take no token: they answer the same without one.
    for (method, path) in PUBLIC_ROUTES {
        let response = app
            .http
            .request(
                Method::from_bytes(method.as_bytes()).unwrap(),
                app.url(&fill(path)),
            )
            .send()
            .await
            .unwrap();
        assert_ne!(
            response.status().as_u16(),
            401,
            "{method} {path} is not public"
        );
    }
    app.server.shutdown().await.unwrap();
}

#[sqlx::test]
async fn each_table_route_needs_exactly_its_scope(pool: PgPool) {
    let app = App::start(pool).await;
    let _owner = app.register("owner").await;
    let alice = app.register("alice").await;
    // One token per scope, holding only that scope.
    let mut by_scope = HashMap::new();
    for scope in TokenScope::ALL {
        by_scope.insert(scope, token_with(&app, &alice, &[scope]).await);
    }
    for route in TOKEN_ROUTES {
        for (scope, secret) in &by_scope {
            let response = app
                .http
                .request(
                    Method::from_bytes(route.method.as_bytes()).unwrap(),
                    app.url(&fill(route.path)),
                )
                .bearer_auth(secret)
                .send()
                .await
                .unwrap();
            let status = response.status().as_u16();
            // An API error, or for /mcp a JSON-RPC error.
            let error = response.json::<Value>().await.ok();
            let message = error
                .as_ref()
                .and_then(|e| e["message"].as_str().or(e["error"]["message"].as_str()))
                .unwrap_or("");
            let scope_refusal =
                status == 403 && message.contains(&format!("the {} scope", route.scope.as_str()));
            assert_eq!(
                scope_refusal,
                *scope != route.scope,
                "{} {} with only {}: {status} {:?}",
                route.method,
                route.path,
                scope.as_str(),
                error
            );
        }
    }
    app.server.shutdown().await.unwrap();
}

// --- The document ---------------------------------------------------------------------------

async fn document(app: &App) -> Value {
    let response = app
        .http
        .get(app.url(doc_paths::OPENAPI_ROUTE))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("application/json"))
    );
    response.json().await.unwrap()
}

#[sqlx::test]
async fn the_description_is_public_valid_and_matches_the_table(pool: PgPool) {
    let app = App::start(pool).await;
    let doc = document(&app).await;
    assert_eq!(doc["openapi"], "3.1.0");
    assert!(doc["info"]["title"].is_string() && doc["info"]["version"].is_string());
    assert_eq!(doc["x-nothing"], Value::Null);
    assert_eq!(
        doc["components"]["securitySchemes"]["bearerAuth"]["scheme"],
        "bearer"
    );

    // The operations are exactly the table's.
    let mut described = HashSet::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for (method, operation) in item.as_object().unwrap() {
            assert!(operation["operationId"].is_string(), "{method} {path}");
            assert!(operation["responses"]["default"].is_object());
            let scope = operation["x-required-scope"].as_str().unwrap();
            assert!(TokenScope::parse(scope).is_some(), "{scope}");
            described.insert((method.to_ascii_uppercase(), path.clone()));
        }
    }
    let table: HashSet<(String, String)> = TOKEN_ROUTES
        .iter()
        .map(|r| (r.method.to_owned(), r.path.to_owned()))
        .collect();
    assert_eq!(described, table);

    // Every scope a token can hold is explained.
    for scope in TokenScope::ALL {
        assert!(
            doc["components"]["securitySchemes"]["bearerAuth"]["x-scopes"][scope.as_str()]
                .is_string(),
            "{}",
            scope.as_str()
        );
    }
    // Query parameters are listed for the routes that take them.
    let params = doc["paths"][doc_paths::WORKSPACE_PAGES_ROUTE]["get"]["parameters"]
        .as_array()
        .unwrap();
    for name in [
        "workspace_id",
        "q",
        "kind",
        "state",
        "sort",
        "limit",
        "offset",
    ] {
        assert!(params.iter().any(|p| p["name"] == name), "{name}");
    }
    app.server.shutdown().await.unwrap();
}

/// A small JSON Schema check: enough of the vocabulary the description uses (`$ref`, `type`,
/// `enum`, `const`, `oneOf`, `required`, `properties`, `items`) to hold real answers against it.
fn conforms(schema: &Value, value: &Value, components: &Value, at: &str) -> Result<(), String> {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let name = reference.trim_start_matches("#/components/schemas/");
        return conforms(&components[name], value, components, at);
    }
    if let Some(options) = schema.get("oneOf").and_then(Value::as_array) {
        let ok = options
            .iter()
            .filter(|option| conforms(option, value, components, at).is_ok())
            .count();
        return if ok == 1 {
            Ok(())
        } else {
            Err(format!("{at}: matches {ok} of oneOf"))
        };
    }
    if let Some(constant) = schema.get("const") {
        return if constant == value {
            Ok(())
        } else {
            Err(format!("{at}: not {constant}"))
        };
    }
    if let Some(variants) = schema.get("enum").and_then(Value::as_array)
        && !variants.contains(value)
    {
        return Err(format!("{at}: {value} is not one of {variants:?}"));
    }
    let types: Vec<&str> = match schema.get("type") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).collect(),
        _ => vec![],
    };
    if !types.is_empty() {
        let actual = match value {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        };
        if !types.contains(&actual) && !(actual == "integer" && types.contains(&"number")) {
            return Err(format!("{at}: {actual} where {types:?}"));
        }
    }
    if let Some(object) = value.as_object() {
        for name in schema
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let name = name.as_str().unwrap();
            if !object.contains_key(name) {
                return Err(format!("{at}: missing {name}"));
            }
        }
        if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
            for (name, inner) in object {
                // Fields the description does not know are a mismatch: the schema must be complete.
                let Some(property) = properties.get(name) else {
                    return Err(format!("{at}: {name} is not described"));
                };
                conforms(property, inner, components, &format!("{at}.{name}"))?;
            }
        }
    }
    if let (Some(items), Some(array)) = (schema.get("items"), value.as_array()) {
        for (index, item) in array.iter().enumerate() {
            conforms(items, item, components, &format!("{at}[{index}]"))?;
        }
    }
    Ok(())
}

#[sqlx::test]
async fn what_the_server_answers_conforms_to_the_description(pool: PgPool) {
    let app = App::start(pool).await;
    let _owner = app.register("owner").await;
    let alice = app.register("alice").await;
    let doc = document(&app).await;
    let components = &doc["components"]["schemas"];
    let check = |name: &str, value: &Value| {
        if let Err(problem) = conforms(&components[name], value, components, name) {
            panic!("{problem}\n{value:#}");
        }
    };

    // A page and a board, listed, read, and with versions.
    let page = PageId::generate();
    let created = app
        .call(
            Method::POST,
            &doc_paths::conversation_pages(app.general),
            &alice,
            Some(json!({
                "operation_id": OperationId::generate(), "page_id": page, "kind": "page",
                "title": "Plan", "content": {"kind": "page", "body": "alpha beta gamma"},
            })),
        )
        .await;
    let created: Value = created.json().await.unwrap();
    check("PageResponse", &created);
    let edited = app
        .call(
            Method::PATCH,
            &doc_paths::page(page),
            &alice,
            Some(json!({
                "operation_id": OperationId::generate(), "base_revision": 1, "title": "Plan 2",
                "content": {"kind": "page", "body": "alpha beta gamma delta"},
            })),
        )
        .await;
    check("PageResponse", &edited.json().await.unwrap());
    let board = app
        .call(
            Method::POST,
            &doc_paths::conversation_pages(app.general),
            &alice,
            Some(json!({
                "operation_id": OperationId::generate(), "page_id": PageId::generate(), "kind": "board",
                "title": "Flow",
                "content": {"kind": "board", "board": {
                    "nodes": [
                        {"id": "a", "shape": "rectangle", "x": 0, "y": 0, "width": 100, "height": 50, "text": "Start", "color": "green"},
                        {"id": "b", "shape": "ellipse", "x": 200, "y": 0, "width": 100, "height": 50}
                    ],
                    "edges": [{"id": "e", "source": "a", "target": "b", "label": "then"}],
                    "viewport": {"x": 0, "y": 0, "zoom_percent": 100}
                }},
            })),
        )
        .await;
    check("PageResponse", &board.json().await.unwrap());
    let list = app
        .call(
            Method::GET,
            &doc_paths::workspace_pages(app.workspace_id),
            &alice,
            None,
        )
        .await;
    check("PageListResponse", &list.json().await.unwrap());
    let versions = app
        .call(Method::GET, &doc_paths::page_versions(page), &alice, None)
        .await;
    let versions: Value = versions.json().await.unwrap();
    check("PageVersionsResponse", &versions);
    let version = app
        .call(Method::GET, &doc_paths::page_version(page, 1), &alice, None)
        .await;
    check("PageVersionResponse", &version.json().await.unwrap());

    // Notations, with a reply and a completion.
    let notation = lazy_domain::NotationId::generate();
    let made = app
        .call(
            Method::POST,
            &doc_paths::page_notations(page),
            &alice,
            Some(json!({
                "operation_id": OperationId::generate(), "notation_id": notation,
                "quote": "beta", "from": 6, "to": 10, "body": "check this",
            })),
        )
        .await;
    check("NotationResponse", &made.json().await.unwrap());
    let replied = app
        .call(
            Method::POST,
            &doc_paths::notation_replies(notation),
            &alice,
            Some(json!({
                "operation_id": OperationId::generate(),
                "reply_id": lazy_domain::NotationReplyId::generate(), "body": "done soon",
            })),
        )
        .await;
    check("NotationResponse", &replied.json().await.unwrap());
    let completed = app
        .call(
            Method::PUT,
            &doc_paths::notation_completed(notation),
            &alice,
            Some(json!({"operation_id": OperationId::generate(), "completed": true})),
        )
        .await;
    check("NotationResponse", &completed.json().await.unwrap());
    let listed = app
        .call(Method::GET, &doc_paths::page_notations(page), &alice, None)
        .await;
    check("NotationListResponse", &listed.json().await.unwrap());

    // The profile, the conversation list, and an error.
    let me = app.call(Method::GET, "/v1/me", &alice, None).await;
    check("ProfileResponse", &me.json().await.unwrap());
    let conversations = app
        .call(
            Method::GET,
            &lazy_protocol::api::paths::open_conversation(app.workspace_id),
            &alice,
            None,
        )
        .await;
    check(
        "ConversationListResponse",
        &conversations.json().await.unwrap(),
    );
    let missing = app
        .call(
            Method::GET,
            &doc_paths::page(PageId::generate()),
            &alice,
            None,
        )
        .await;
    check("ApiError", &missing.json().await.unwrap());
    app.server.shutdown().await.unwrap();
}
