//! The OpenAPI 3.1 description of the routes an access token can reach (llm-docs/DECISIONS.md,
//! D31), served at `GET /v1/openapi.json`.
//!
//! The paths, methods, scopes, summaries, and body and response schema names come from
//! [`crate::token_routes::TOKEN_ROUTES`], the table that also decides what a token may do, so the
//! description cannot list a route a token cannot reach or leave one out. The component schemas
//! are written by hand here, in one place: `tests/openapi.rs` checks that every name the table
//! uses is defined, and that every example the protocol crate serializes validates against its
//! schema's required fields.

use lazy_protocol::{PROTOCOL_VERSION, TokenScope};
use serde_json::{Map, Value, json};

use crate::token_routes::{TOKEN_ROUTES, TokenRoute};

fn reference(name: &str) -> Value {
    json!({ "$ref": format!("#/components/schemas/{name}") })
}

fn string(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

fn uuid() -> Value {
    json!({ "type": "string", "format": "uuid" })
}

fn time() -> Value {
    json!({ "type": "string", "format": "date-time" })
}

fn integer() -> Value {
    json!({ "type": "integer", "minimum": 0 })
}

fn boolean() -> Value {
    json!({ "type": "boolean" })
}

fn array(items: Value) -> Value {
    json!({ "type": "array", "items": items })
}

fn object(required: &[&str], properties: Vec<(&str, Value)>) -> Value {
    let mut map = Map::new();
    for (name, schema) in properties {
        map.insert(name.to_owned(), schema);
    }
    json!({ "type": "object", "required": required, "properties": map })
}

fn variants(names: &[&str]) -> Value {
    json!({ "type": "string", "enum": names })
}

/// The component schemas, by name.
pub fn schemas() -> Map<String, Value> {
    let mut s = Map::new();
    let mut add = |name: &str, schema: Value| {
        s.insert(name.to_owned(), schema);
    };

    add(
        "ApiError",
        object(
            &["code", "message"],
            vec![
                (
                    "code",
                    variants(&[
                        "invalid_request",
                        "unauthenticated",
                        "invalid_credentials",
                        "forbidden",
                        "not_found",
                        "conflict",
                        "stale_revision",
                        "username_taken",
                        "name_taken",
                        "invitation_invalid",
                        "payload_too_large",
                        "unsupported_media",
                        "limit_reached",
                        "rate_limited",
                        "internal",
                        "unavailable",
                    ]),
                ),
                ("message", string("A human-readable explanation.")),
            ],
        ),
    );

    add("PageKind", variants(&["page", "board"]));
    add(
        "BoardNode",
        object(
            &["id", "shape", "x", "y", "width", "height"],
            vec![
                ("id", string("Unique on the board; 1 to 64 bytes.")),
                (
                    "shape",
                    variants(&["rectangle", "ellipse", "diamond", "note", "text"]),
                ),
                ("x", json!({"type": "integer"})),
                ("y", json!({"type": "integer"})),
                ("width", json!({"type": "integer", "minimum": 1})),
                ("height", json!({"type": "integer", "minimum": 1})),
                ("text", json!({"type": "string", "maxLength": 2000})),
                (
                    "color",
                    variants(&[
                        "neutral", "red", "orange", "yellow", "green", "blue", "purple",
                    ]),
                ),
            ],
        ),
    );
    add(
        "BoardEdge",
        object(
            &["id", "source", "target"],
            vec![
                ("id", string("Unique among connectors.")),
                ("source", string("The id of the shape it leaves.")),
                ("target", string("The id of the shape it arrives at.")),
                ("label", string("At most 200 bytes.")),
            ],
        ),
    );
    add(
        "Viewport",
        object(
            &["x", "y", "zoom_percent"],
            vec![
                ("x", json!({"type": "integer"})),
                ("y", json!({"type": "integer"})),
                (
                    "zoom_percent",
                    json!({"type": "integer", "minimum": 10, "maximum": 400}),
                ),
            ],
        ),
    );
    add(
        "Board",
        object(
            &[],
            vec![
                (
                    "nodes",
                    json!({"type": "array", "maxItems": 500, "items": reference("BoardNode")}),
                ),
                (
                    "edges",
                    json!({"type": "array", "maxItems": 1000, "items": reference("BoardEdge")}),
                ),
                ("viewport", reference("Viewport")),
            ],
        ),
    );
    add(
        "PageContent",
        json!({
            "description": "Markdown text for a page, a canvas for a board; matches the page's kind.",
            "oneOf": [
                object(
                    &["kind", "body"],
                    vec![
                        ("kind", json!({"const": "page"})),
                        (
                            "body",
                            json!({"type": "string", "description": "Markdown, at most 1,000,000 bytes."}),
                        ),
                    ],
                ),
                object(
                    &["kind", "board"],
                    vec![
                        ("kind", json!({"const": "board"})),
                        ("board", reference("Board")),
                    ],
                ),
            ],
            "discriminator": { "propertyName": "kind" },
        }),
    );

    add(
        "Attachment",
        object(
            &["id", "name", "media_type", "size", "sha256"],
            vec![
                ("id", uuid()),
                ("name", string("The file name, made safe.")),
                ("media_type", string("What the server decided the file is.")),
                ("size", integer()),
                ("sha256", string("Lowercase hex SHA-256 of the bytes.")),
                ("width", integer()),
                ("height", integer()),
                ("has_thumbnail", boolean()),
            ],
        ),
    );
    add(
        "AttachmentResponse",
        object(
            &["attachment"],
            vec![("attachment", reference("Attachment"))],
        ),
    );

    let summary_fields = vec![
        ("id", uuid()),
        ("workspace_id", uuid()),
        ("conversation_id", uuid()),
        ("kind", reference("PageKind")),
        (
            "title",
            string("1 to 255 characters; \"Untitled\" when made with none."),
        ),
        (
            "excerpt",
            string("The first 160 characters of the text, on one line."),
        ),
        ("created_by", uuid()),
        ("created_at", time()),
        ("updated_by", uuid()),
        ("updated_at", time()),
        (
            "revision",
            json!({
                "type": "integer", "minimum": 1,
                "description": "Rises by one with every change. Send it back as base_revision."
            }),
        ),
        ("trashed_at", time()),
        ("purge_at", time()),
        ("open_notations", integer()),
        ("file_count", integer()),
    ];
    let summary_required = [
        "id",
        "workspace_id",
        "conversation_id",
        "kind",
        "title",
        "created_by",
        "created_at",
        "updated_by",
        "updated_at",
        "revision",
    ];
    add(
        "PageSummary",
        object(&summary_required, summary_fields.clone()),
    );
    let mut detail_fields = summary_fields;
    detail_fields.push(("content", reference("PageContent")));
    detail_fields.push(("files", array(reference("Attachment"))));
    let mut detail_required = summary_required.to_vec();
    detail_required.push("content");
    add("PageDetail", object(&detail_required, detail_fields));
    add(
        "PageResponse",
        object(&["page"], vec![("page", reference("PageDetail"))]),
    );
    add(
        "CreatePageRequest",
        object(
            &["operation_id", "page_id", "kind"],
            vec![
                ("operation_id", uuid()),
                ("page_id", uuid()),
                ("kind", reference("PageKind")),
                ("title", string("Empty means \"Untitled\".")),
                ("content", reference("PageContent")),
            ],
        ),
    );
    add(
        "UpdatePageRequest",
        object(
            &["operation_id", "base_revision"],
            vec![
                ("operation_id", uuid()),
                (
                    "base_revision",
                    json!({"type": "integer", "minimum": 1,
                           "description": "stale_revision if the page has changed since."}),
                ),
                ("title", json!({"type": "string"})),
                ("content", reference("PageContent")),
                (
                    "append",
                    string("Text added after the page's text; pages only, not with content."),
                ),
                (
                    "conversation_id",
                    json!({"type": "string", "format": "uuid",
                           "description": "Move the page to another conversation of the workspace."}),
                ),
            ],
        ),
    );
    add(
        "PageActionRequest",
        object(&["operation_id"], vec![("operation_id", uuid())]),
    );
    add(
        "PageListParams",
        object(
            &[],
            vec![
                ("state", variants(&["live", "trashed"])),
                ("q", string("Words to find in titles and text.")),
                ("kind", reference("PageKind")),
                ("sort", variants(&["updated", "title"])),
                (
                    "limit",
                    json!({"type": "integer", "minimum": 1, "maximum": 100}),
                ),
                ("offset", integer()),
            ],
        ),
    );
    add(
        "PageListResponse",
        object(
            &["pages", "has_more"],
            vec![
                ("pages", array(reference("PageSummary"))),
                ("has_more", boolean()),
            ],
        ),
    );

    let version_fields = vec![
        ("page_id", uuid()),
        ("version", json!({"type": "integer", "minimum": 1})),
        ("title", json!({"type": "string"})),
        ("reason", variants(&["autosave", "pre_restore"])),
        ("created_by", uuid()),
        ("created_at", time()),
        ("revision", json!({"type": "integer", "minimum": 1})),
    ];
    let version_required = [
        "page_id",
        "version",
        "title",
        "reason",
        "created_at",
        "revision",
    ];
    add(
        "PageVersionSummary",
        object(&version_required, version_fields.clone()),
    );
    let mut version_detail = version_fields;
    version_detail.push(("content", reference("PageContent")));
    let mut version_detail_required = version_required.to_vec();
    version_detail_required.push("content");
    add(
        "PageVersionDetail",
        object(&version_detail_required, version_detail),
    );
    add(
        "PageVersionsResponse",
        object(
            &["versions"],
            vec![("versions", array(reference("PageVersionSummary")))],
        ),
    );
    add(
        "PageVersionResponse",
        object(
            &["version"],
            vec![("version", reference("PageVersionDetail"))],
        ),
    );
    add(
        "RestoreVersionRequest",
        object(
            &["operation_id", "base_revision"],
            vec![
                ("operation_id", uuid()),
                ("base_revision", json!({"type": "integer", "minimum": 1})),
            ],
        ),
    );

    add(
        "NotationReply",
        object(
            &["id", "notation_id", "author_id", "body", "created_at"],
            vec![
                ("id", uuid()),
                ("notation_id", uuid()),
                ("author_id", uuid()),
                ("body", string("At most 5,000 bytes.")),
                ("created_at", time()),
            ],
        ),
    );
    add(
        "Notation",
        object(
            &[
                "id",
                "page_id",
                "conversation_id",
                "author_id",
                "assignee_id",
                "quote",
                "from",
                "to",
                "body",
                "created_at",
                "updated_at",
            ],
            vec![
                ("id", uuid()),
                ("page_id", uuid()),
                ("conversation_id", uuid()),
                ("author_id", uuid()),
                ("assignee_id", uuid()),
                (
                    "quote",
                    string("The text the note was made on; at most 2,000 bytes."),
                ),
                (
                    "from",
                    json!({"type": "integer", "minimum": 0,
                           "description": "Start of the quote in the page text, in characters."}),
                ),
                ("to", json!({"type": "integer", "minimum": 1})),
                ("body", string("At most 5,000 bytes.")),
                ("created_at", time()),
                ("updated_at", time()),
                ("completed_at", time()),
                ("resolved_by", uuid()),
                ("replies", array(reference("NotationReply"))),
            ],
        ),
    );
    add(
        "CreateNotationRequest",
        object(
            &["operation_id", "notation_id", "quote", "from", "to", "body"],
            vec![
                ("operation_id", uuid()),
                ("notation_id", uuid()),
                ("quote", json!({"type": "string"})),
                ("from", integer()),
                ("to", integer()),
                ("body", json!({"type": "string"})),
                ("assignee_id", uuid()),
            ],
        ),
    );
    add(
        "UpdateNotationRequest",
        object(
            &["operation_id"],
            vec![
                ("operation_id", uuid()),
                ("body", json!({"type": "string"})),
                ("assignee_id", uuid()),
            ],
        ),
    );
    add(
        "SetNotationCompletedRequest",
        object(
            &["operation_id", "completed"],
            vec![("operation_id", uuid()), ("completed", boolean())],
        ),
    );
    add(
        "CreateNotationReplyRequest",
        object(
            &["operation_id", "reply_id", "body"],
            vec![
                ("operation_id", uuid()),
                ("reply_id", uuid()),
                ("body", json!({"type": "string"})),
            ],
        ),
    );
    add(
        "NotationActionRequest",
        object(&["operation_id"], vec![("operation_id", uuid())]),
    );
    add(
        "NotationResponse",
        object(&["notation"], vec![("notation", reference("Notation"))]),
    );
    add(
        "NotationListResponse",
        object(
            &["notations"],
            vec![("notations", array(reference("Notation")))],
        ),
    );
    add(
        "NotationListParams",
        object(
            &[],
            vec![
                ("scope", variants(&["assigned", "created"])),
                (
                    "completed",
                    json!({"type": "boolean", "description": "Show completed notations instead of open ones."}),
                ),
                ("q", json!({"type": "string"})),
            ],
        ),
    );

    add(
        "User",
        object(
            &["id", "username", "display_name"],
            vec![
                ("id", uuid()),
                ("username", json!({"type": "string"})),
                ("display_name", json!({"type": "string"})),
                ("is_bot", boolean()),
                ("avatar_version", integer()),
            ],
        ),
    );
    add(
        "ProfileResponse",
        object(&["user"], vec![("user", reference("User"))]),
    );
    add(
        "Conversation",
        object(
            &[
                "id",
                "workspace_id",
                "kind",
                "visibility",
                "archived",
                "member_count",
            ],
            vec![
                ("id", uuid()),
                ("workspace_id", uuid()),
                ("kind", variants(&["channel", "direct", "group"])),
                ("name", json!({"type": ["string", "null"]})),
                ("topic", json!({"type": ["string", "null"]})),
                ("description", json!({"type": ["string", "null"]})),
                ("visibility", variants(&["public", "private"])),
                ("archived", boolean()),
                ("is_default", boolean()),
                (
                    "created_by",
                    json!({"type": ["string", "null"], "format": "uuid"}),
                ),
                ("participants", array(uuid())),
                ("member_count", integer()),
            ],
        ),
    );
    add(
        "ConversationListResponse",
        object(
            &["conversations"],
            vec![("conversations", array(reference("Conversation")))],
        ),
    );
    add(
        "BrowseChannelsResponse",
        object(
            &["channels"],
            vec![
                (
                    "channels",
                    array(json!({
                        "type": "object",
                        "description": "A public channel and whether the caller has joined it."
                    })),
                ),
                ("has_more", boolean()),
            ],
        ),
    );
    s
}

fn scope_description() -> String {
    let mut text = String::from(
        "Everything the Lazy docs feature does is reachable with a personal access token, sent as \
         `Authorization: Bearer lzp_...`. A token has the access of its owner, limited to its \
         scopes, and reaches only the routes listed here. Make tokens in the app; a token cannot \
         make tokens. Every mutation takes an `operation_id`: repeating a request with the same \
         id returns the original result, and the same id with a different body is `conflict`.\n\n\
         Scopes:\n",
    );
    for scope in TokenScope::ALL {
        text.push_str(&format!(
            "- `{}`: {}\n",
            scope.as_str(),
            scope_summary(scope)
        ));
    }
    text
}

fn scope_summary(scope: TokenScope) -> &'static str {
    match scope {
        TokenScope::ProfileRead => "read the owner's profile",
        TokenScope::PagesRead => "read pages, boards, versions, and their files",
        TokenScope::PagesWrite => "create, edit, move, trash, restore, and purge pages; add files",
        TokenScope::NotationsRead => "read notations",
        TokenScope::NotationsWrite => "make, change, complete, reply to, and delete notations",
        TokenScope::ConversationsRead => "list the conversations the owner can read",
        TokenScope::McpUse => "speak the Model Context Protocol at /mcp",
    }
}

/// The names `{like_this}` in a route template.
fn placeholders(path: &str) -> Vec<&str> {
    path.split('{')
        .skip(1)
        .filter_map(|rest| rest.split_once('}').map(|(name, _)| name))
        .collect()
}

fn path_parameter(name: &str) -> Value {
    let schema = if name == "version" {
        json!({"type": "integer", "minimum": 1})
    } else {
        json!({"type": "string", "format": "uuid"})
    };
    json!({ "name": name, "in": "path", "required": true, "schema": schema })
}

fn operation(route: &TokenRoute, all: &Map<String, Value>) -> Value {
    let mut parameters: Vec<Value> = placeholders(route.path)
        .into_iter()
        .map(path_parameter)
        .collect();
    if let Some(query) = route.query
        && let Some(properties) = all
            .get(query)
            .and_then(|schema| schema.get("properties"))
            .and_then(Value::as_object)
    {
        for (name, schema) in properties {
            parameters.push(json!({ "name": name, "in": "query", "schema": schema }));
        }
    }
    if route.path.contains("/files/") {
        parameters.push(json!({
            "name": "x-attachment-name", "in": "header", "required": true,
            "description": "The file's name, percent-encoded UTF-8.",
            "schema": { "type": "string" }
        }));
        parameters.push(json!({
            "name": "content-length", "in": "header", "required": true,
            "schema": { "type": "integer" }
        }));
    }

    let mut op = Map::new();
    op.insert("operationId".into(), json!(route.operation_id));
    op.insert("summary".into(), json!(route.summary));
    op.insert("tags".into(), json!([tag(route)]));
    op.insert("parameters".into(), Value::Array(parameters));
    op.insert(
        "security".into(),
        json!([{ "bearerAuth": [route.scope.as_str()] }]),
    );
    op.insert("x-required-scope".into(), json!(route.scope.as_str()));
    if let Some(request) = route.request {
        op.insert(
            "requestBody".into(),
            json!({
                "required": true,
                "content": { "application/json": { "schema": reference(request) } }
            }),
        );
    } else if route.path.contains("/files/") {
        op.insert(
            "requestBody".into(),
            json!({
                "required": true,
                "content": { "application/octet-stream": { "schema": { "type": "string", "format": "binary" } } }
            }),
        );
    } else if route.path == "/mcp" {
        op.insert(
            "requestBody".into(),
            json!({
                "required": true,
                "description": "One JSON-RPC 2.0 request or notification.",
                "content": { "application/json": { "schema": { "type": "object" } } }
            }),
        );
    }

    let mut responses = Map::new();
    let success = if route.status == 204 {
        json!({ "description": "Done." })
    } else if let Some(response) = route.response {
        json!({
            "description": "Success.",
            "content": { "application/json": { "schema": reference(response) } }
        })
    } else if route.path == "/mcp" {
        json!({
            "description": "The JSON-RPC response; a notification gets 202 with no body.",
            "content": { "application/json": { "schema": { "type": "object" } } }
        })
    } else {
        json!({
            "description": "The file's bytes.",
            "content": { "*/*": { "schema": { "type": "string", "format": "binary" } } }
        })
    };
    responses.insert(route.status.to_string(), success);
    responses.insert("default".into(), error_response());
    op.insert("responses".into(), Value::Object(responses));
    Value::Object(op)
}

fn error_response() -> Value {
    json!({
        "description": "An error. 401 `unauthenticated`: no valid token. 403 `forbidden`: the \
                        token lacks the scope, or the owner may not do this. 404 `not_found`: it \
                        does not exist or the owner may not know it does. 409 `stale_revision` or \
                        `conflict`: the page changed first, or the operation id was used for \
                        something else.",
        "content": { "application/json": { "schema": reference("ApiError") } }
    })
}

fn tag(route: &TokenRoute) -> &'static str {
    let path = route.path;
    if path == "/mcp" {
        "MCP"
    } else if path.contains("notation") {
        "Notations"
    } else if path.contains("/pages") || path.contains("/attachments") {
        "Pages"
    } else if path.contains("/me") {
        "Profile"
    } else {
        "Conversations"
    }
}

/// The whole description.
pub fn document() -> Value {
    let all = schemas();
    let mut paths: Map<String, Value> = Map::new();
    for route in TOKEN_ROUTES {
        let item = paths
            .entry(route.path.to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        if let Value::Object(item) = item {
            item.insert(route.method.to_ascii_lowercase(), operation(route, &all));
        }
    }
    let scopes: Map<String, Value> = TokenScope::ALL
        .into_iter()
        .map(|scope| (scope.as_str().to_owned(), json!(scope_summary(scope))))
        .collect();
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Lazy docs API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": scope_description(),
            "x-protocol-version": PROTOCOL_VERSION,
        },
        "servers": [{ "url": "/" }],
        "tags": [
            { "name": "Pages" }, { "name": "Notations" }, { "name": "Conversations" },
            { "name": "Profile" }, { "name": "MCP" }
        ],
        "paths": paths,
        "components": {
            "securitySchemes": {
                "bearerAuth": {
                    "type": "http",
                    "scheme": "bearer",
                    "description": "A personal access token (`lzp_...`). The operation's \
                                    `security` entry names the scope it needs.",
                    "x-scopes": scopes,
                }
            },
            "schemas": all,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect_refs(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (key, inner) in map {
                    if key == "$ref"
                        && let Some(text) = inner.as_str()
                    {
                        out.push(text.to_owned());
                    }
                    collect_refs(inner, out);
                }
            }
            Value::Array(items) => items.iter().for_each(|item| collect_refs(item, out)),
            _ => {}
        }
    }

    #[test]
    fn every_reference_points_at_a_defined_schema() {
        let doc = document();
        let mut refs = Vec::new();
        collect_refs(&doc, &mut refs);
        assert!(!refs.is_empty());
        for reference in refs {
            let name = reference
                .strip_prefix("#/components/schemas/")
                .unwrap_or_else(|| panic!("{reference} is not a component reference"));
            assert!(
                doc["components"]["schemas"].get(name).is_some(),
                "{name} is referenced but not defined"
            );
        }
    }

    #[test]
    fn every_table_route_is_described_once_with_its_scope() {
        let doc = document();
        for route in TOKEN_ROUTES {
            let operation = &doc["paths"][route.path][route.method.to_ascii_lowercase()];
            assert_eq!(operation["operationId"], route.operation_id);
            assert_eq!(operation["x-required-scope"], route.scope.as_str());
            assert_eq!(
                operation["security"][0]["bearerAuth"][0],
                route.scope.as_str()
            );
            // Every placeholder is a declared path parameter.
            for name in placeholders(route.path) {
                assert!(
                    operation["parameters"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|p| p["name"] == name && p["in"] == "path"),
                    "{} lacks {name}",
                    route.path
                );
            }
        }
    }
}
