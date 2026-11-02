//! The Model Context Protocol server (llm-docs/DECISIONS.md, D31): `POST /mcp`, JSON-RPC 2.0 over
//! the Streamable HTTP transport, answering every request with one JSON object (never an SSE
//! stream), for a bearer access token that holds `mcp:use`.
//!
//! # Protocol revisions
//!
//! Checked against modelcontextprotocol.io on 2026-10-31. The newest revision is **2026-07-28**,
//! which has no `initialize` handshake and no sessions: every request carries its protocol version
//! in `params._meta["io.modelcontextprotocol/protocolVersion"]` and in the `MCP-Protocol-Version`
//! header, plus `Mcp-Method` (and `Mcp-Name` for `tools/call`) headers that must match the body.
//! Clients that do not speak it yet use **2025-11-25**, **2025-06-18**, or **2025-03-26**, which
//! open with `initialize`. This server answers both eras on the one endpoint ("dual-era" in the
//! spec's terms), tells them apart by the header, and keeps no state between requests (so it never
//! issues an `MCP-Session-Id`; a client that sends one is ignored).
//!
//! What is deliberately not offered: server-sent events (so `GET` and `DELETE` are `405`, which
//! both eras allow), `subscriptions/listen`, resources, prompts, logging, server-to-client
//! requests, and batching (removed from the protocol in 2025-06-18; a JSON array is refused).
//! `tools/list` is not paginated. A modern request that lacks `clientInfo` or `clientCapabilities`
//! in `_meta` is served anyway; only the protocol version is required.
//!
//! # Authorization
//!
//! Not OAuth (D31): the credential is a personal access token with the `mcp:use` scope, and each
//! tool also needs the scope of what it does. A tool the token's scopes do not allow is left out
//! of `tools/list`, and calling it is a tool error that names the missing scope. Every tool calls
//! the same module function as the REST route, so access, limits, receipts, and events are the
//! same.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::header::{ALLOW, HOST, ORIGIN, WWW_AUTHENTICATE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use lazy_domain::page::PageContent;
use lazy_domain::{ConversationId, NotationId, OperationId, PageId, PageKind, UserId, WorkspaceId};
use lazy_protocol::docs::PageListParams;
use lazy_protocol::{
    CreateNotationRequest, CreatePageRequest, ErrorCode, NotationListParams, NotationResponse,
    NotationScope, PageResponse, RestoreVersionRequest, SetNotationCompletedRequest, TokenScope,
    UpdatePageRequest,
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::access::active_member;
use crate::app::AppState;
use crate::error::AppError;
use crate::http::bearer;
use crate::logging::Describe;
use crate::{conversations, crypto, notations, pages, tokens};

/// Revisions this endpoint speaks, newest first.
pub const SUPPORTED_VERSIONS: [&str; 4] = ["2026-07-28", "2025-11-25", "2025-06-18", "2025-03-26"];
/// The revision that works without `initialize`.
const MODERN: &str = "2026-07-28";
/// What a request without `MCP-Protocol-Version` is taken to be (the spec's fallback).
const ASSUMED: &str = "2025-03-26";
/// The newest revision that has `initialize`, offered to a client that asks for one we lack.
const NEWEST_LEGACY: &str = "2025-11-25";
const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;
/// Authentication failed (outside the range JSON-RPC reserves).
const UNAUTHORIZED: i64 = -32001;
const HEADER_MISMATCH: i64 = -32020;
const UNSUPPORTED_VERSION: i64 = -32022;

/// Most results a search returns.
const MAX_SEARCH_RESULTS: usize = 50;
const DEFAULT_SEARCH_RESULTS: usize = 20;

const INSTRUCTIONS: &str = "Lazy docs: pages (markdown) and boards that live in conversations. \
Use list_conversations to find where pages live, search to find pages, fetch to read one. \
update_page needs the base_revision from fetch and is refused with stale_revision if the page \
changed since; fetch again and retry. Every write takes an optional idempotency_key: repeating a \
call with the same key does not repeat the write.";

// --- JSON-RPC plumbing --------------------------------------------------------------------------

fn rpc_error(
    status: StatusCode,
    id: Value,
    code: i64,
    message: &str,
    data: Option<Value>,
) -> Response {
    let mut error = json!({ "code": code, "message": message });
    if let Some(data) = data {
        error["data"] = data;
    }
    (
        status,
        axum::Json(json!({ "jsonrpc": "2.0", "id": id, "error": error })),
    )
        .into_response()
}

fn rpc_result(id: Value, result: Value) -> Response {
    axum::Json(json!({ "jsonrpc": "2.0", "id": id, "result": result })).into_response()
}

fn accepted() -> Response {
    StatusCode::ACCEPTED.into_response()
}

/// `GET` and `DELETE`: this endpoint offers no event stream and no sessions.
pub(crate) async fn not_allowed() -> Response {
    let mut response = StatusCode::METHOD_NOT_ALLOWED.into_response();
    response
        .headers_mut()
        .insert(ALLOW, HeaderValue::from_static("POST"));
    response
}

/// Whether the `Origin` header, if there is one, is this server's own host. The spec requires a
/// server to refuse a foreign `Origin` (a defence against DNS rebinding); a client that is not a
/// browser sends none.
fn origin_is_acceptable(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(ORIGIN) else {
        return true;
    };
    let Some(host) = headers.get(HOST).and_then(|host| host.to_str().ok()) else {
        return false;
    };
    let Some(origin) = origin.to_str().ok() else {
        return false;
    };
    let authority = origin
        .split_once("://")
        .map(|(_, rest)| rest.trim_end_matches('/'));
    authority.is_some_and(|authority| authority.eq_ignore_ascii_case(host))
}

/// Standard base64 (RFC 4648), padded, for the `=?base64?...?=` form of `Mcp-Name`.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut buffer = 0u32;
    let mut bits = 0;
    for byte in text.trim_end_matches('=').bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// A header value as the text it stands for (`Mcp-Name` may be sent base64-encoded).
fn header_text(value: &HeaderValue) -> Option<String> {
    let text = value.to_str().ok()?;
    if let Some(encoded) = text
        .strip_prefix("=?base64?")
        .and_then(|rest| rest.strip_suffix("?="))
    {
        return String::from_utf8(base64_decode(encoded)?).ok();
    }
    Some(text.to_owned())
}

// --- The endpoint ---------------------------------------------------------------------------------

/// `POST /mcp`.
pub(crate) async fn post(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !origin_is_acceptable(&headers) {
        return rpc_error(
            StatusCode::FORBIDDEN,
            Value::Null,
            UNAUTHORIZED,
            "the Origin of this request is not this server",
            None,
        );
    }
    let auth = match authenticate(&state, &headers).await {
        Ok(auth) => auth,
        Err(response) => return *response,
    };

    let message: Value = match serde_json::from_slice(&body) {
        Ok(message) => message,
        Err(_) => {
            return rpc_error(
                StatusCode::BAD_REQUEST,
                Value::Null,
                PARSE_ERROR,
                "the body is not JSON",
                None,
            );
        }
    };
    let Some(object) = message.as_object() else {
        return rpc_error(
            StatusCode::BAD_REQUEST,
            Value::Null,
            INVALID_REQUEST,
            "send one JSON-RPC request or notification; batches are not accepted",
            None,
        );
    };
    let id = object.get("id").cloned();
    let reply_id = id.clone().unwrap_or(Value::Null);
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return rpc_error(
            StatusCode::BAD_REQUEST,
            reply_id,
            INVALID_REQUEST,
            "\"jsonrpc\" must be \"2.0\"",
            None,
        );
    }

    // Which revision is the client speaking?
    let header_version = headers
        .get("mcp-protocol-version")
        .and_then(|value| value.to_str().ok());
    let version = header_version.unwrap_or(ASSUMED);
    if !SUPPORTED_VERSIONS.contains(&version) {
        return rpc_error(
            StatusCode::BAD_REQUEST,
            reply_id,
            UNSUPPORTED_VERSION,
            "Unsupported protocol version",
            Some(json!({ "supported": SUPPORTED_VERSIONS, "requested": version })),
        );
    }
    let modern = version == MODERN;

    let Some(method) = object.get("method").and_then(Value::as_str) else {
        // A JSON-RPC response from the client: legacy revisions let a client send them (to answer
        // a server request, which this server never makes); the newest does not.
        return if !modern && (object.contains_key("result") || object.contains_key("error")) {
            accepted()
        } else {
            rpc_error(
                StatusCode::BAD_REQUEST,
                reply_id,
                INVALID_REQUEST,
                "a request needs a \"method\"",
                None,
            )
        };
    };
    let params = object.get("params").cloned().unwrap_or(Value::Null);

    if modern && let Err(response) = check_modern_request(&headers, method, &params, &reply_id) {
        return *response;
    }

    // A notification has no id and gets no answer.
    let Some(id) = id else {
        return accepted();
    };

    match method {
        "ping" => rpc_result(id, json!({})),
        "initialize" if !modern => initialize(id, &params),
        "server/discover" if modern => rpc_result(
            id,
            json!({
                "resultType": "complete",
                "supportedVersions": SUPPORTED_VERSIONS,
                "capabilities": { "tools": {} },
                "_meta": { META_SERVER_INFO: server_info() },
                "instructions": INSTRUCTIONS,
            }),
        ),
        "tools/list" => {
            let tools: Vec<Value> = tool_definitions()
                .into_iter()
                .filter(|tool| auth.allows(tool.scope))
                .map(|tool| tool.definition)
                .collect();
            let mut result = json!({ "tools": tools });
            if modern {
                result["resultType"] = json!("complete");
            }
            rpc_result(id, result)
        }
        "tools/call" => call_tool(&state, &auth, id, params, modern).await,
        _ => {
            let status = if modern {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::OK
            };
            rpc_error(status, id, METHOD_NOT_FOUND, "method not found", None)
        }
    }
}

fn server_info() -> Value {
    json!({ "name": "lazy", "title": "Lazy docs", "version": env!("CARGO_PKG_VERSION") })
}

fn initialize(id: Value, params: &Value) -> Response {
    let requested = params.get("protocolVersion").and_then(Value::as_str);
    // The spec: answer with the requested version if supported, else the newest the server has.
    let version = match requested {
        Some(version) if SUPPORTED_VERSIONS[1..].contains(&version) => version,
        _ => NEWEST_LEGACY,
    };
    rpc_result(
        id,
        json!({
            "protocolVersion": version,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": server_info(),
            "instructions": INSTRUCTIONS,
        }),
    )
}

/// The checks the newest revision adds: the version in the body and the `Mcp-Method` and
/// `Mcp-Name` headers must match what the body says.
fn check_modern_request(
    headers: &HeaderMap,
    method: &str,
    params: &Value,
    id: &Value,
) -> Result<(), Box<Response>> {
    let mismatch = |message: &str| {
        Err(Box::new(rpc_error(
            StatusCode::BAD_REQUEST,
            id.clone(),
            HEADER_MISMATCH,
            message,
            None,
        )))
    };
    let body_version = params
        .get("_meta")
        .and_then(|meta| meta.get(META_VERSION))
        .and_then(Value::as_str);
    if body_version != Some(MODERN) {
        return mismatch(&format!(
            "params._meta[\"{META_VERSION}\"] must be \"{MODERN}\", the same as the MCP-Protocol-Version header"
        ));
    }
    let header_method = headers.get("mcp-method").and_then(header_text);
    if header_method.as_deref() != Some(method) {
        return mismatch("the Mcp-Method header is missing or does not match the method");
    }
    if method == "tools/call" {
        let name = params.get("name").and_then(Value::as_str);
        let header_name = headers.get("mcp-name").and_then(header_text);
        if name.is_none() || header_name.as_deref() != name {
            return mismatch("the Mcp-Name header is missing or does not match the tool name");
        }
    }
    Ok(())
}

// --- Authentication ---------------------------------------------------------------------------------

/// A caller that has been let in: whose access it is, and the scopes narrowing it.
struct Caller {
    user: UserId,
    scopes: Vec<TokenScope>,
}

impl Caller {
    fn allows(&self, scope: TokenScope) -> bool {
        self.scopes.contains(&scope)
    }
}

fn unauthorized(message: &str) -> Response {
    let mut response = rpc_error(
        StatusCode::UNAUTHORIZED,
        Value::Null,
        UNAUTHORIZED,
        message,
        None,
    );
    response.headers_mut().insert(
        WWW_AUTHENTICATE,
        HeaderValue::from_static("Bearer realm=\"lazy\""),
    );
    response
}

/// Lets in a token that holds `mcp:use`. A session is not accepted here: the endpoint is for
/// tokens, which a person makes and can revoke without ending their sign-in.
async fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<Caller, Box<Response>> {
    let Ok(secret) = bearer(headers) else {
        return Err(Box::new(unauthorized(
            "send an access token as a Bearer credential",
        )));
    };
    if !crypto::looks_like_access_token(secret) {
        return Err(Box::new(rpc_error(
            StatusCode::FORBIDDEN,
            Value::Null,
            UNAUTHORIZED,
            "this endpoint takes an access token, not a session",
            None,
        )));
    }
    let auth = match tokens::authenticate(&state.pool, secret).await {
        Ok(auth) => auth,
        Err(AppError::Api(_)) => return Err(Box::new(unauthorized("the token is not valid"))),
        Err(_) => {
            return Err(Box::new(rpc_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                Value::Null,
                INTERNAL_ERROR,
                "internal server error",
                None,
            )));
        }
    };
    if !auth.has(TokenScope::McpUse) {
        return Err(Box::new(rpc_error(
            StatusCode::FORBIDDEN,
            Value::Null,
            UNAUTHORIZED,
            "this token does not have the mcp:use scope",
            None,
        )));
    }
    Ok(Caller {
        user: auth.user_id,
        scopes: auth.scopes,
    })
}

// --- Tools -----------------------------------------------------------------------------------------------

struct Tool {
    name: &'static str,
    scope: TokenScope,
    definition: Value,
}

fn tool(
    name: &'static str,
    scope: TokenScope,
    title: &str,
    description: &str,
    properties: Value,
    required: &[&str],
    read_only: bool,
) -> Tool {
    Tool {
        name,
        scope,
        definition: json!({
            "name": name,
            "title": title,
            "description": description,
            "inputSchema": {
                "type": "object",
                "properties": properties,
                "required": required,
                "additionalProperties": false,
            },
            "annotations": { "readOnlyHint": read_only },
        }),
    }
}

fn id_property(description: &str) -> Value {
    json!({ "type": "string", "format": "uuid", "description": description })
}

fn key_property() -> Value {
    json!({
        "type": "string", "maxLength": 100,
        "description": "Optional. A call repeated with the same key does not repeat the write."
    })
}

/// Every tool, in the order `tools/list` gives them (always the same order).
fn tool_definitions() -> Vec<Tool> {
    use TokenScope::{ConversationsRead, NotationsRead, NotationsWrite, PagesRead, PagesWrite};
    vec![
        tool(
            "search",
            PagesRead,
            "Search pages",
            "Find pages and boards by words in their title or text, across the workspaces the \
             token's owner belongs to (or one). Returns summaries; use fetch to read one.",
            json!({
                "query": { "type": "string", "description": "Words to find." },
                "workspace_id": id_property("Limit to one workspace."),
                "kind": { "enum": ["page", "board"] },
                "limit": { "type": "integer", "minimum": 1, "maximum": MAX_SEARCH_RESULTS },
            }),
            &["query"],
            true,
        ),
        tool(
            "fetch",
            PagesRead,
            "Read a page",
            "Read one page or board: title, content (markdown for a page, shapes for a board), \
             revision, and files.",
            json!({ "page_id": id_property("The page.") }),
            &["page_id"],
            true,
        ),
        tool(
            "create_page",
            PagesWrite,
            "Create a page",
            "Make a page of markdown in a conversation the token's owner can post in. Boards are \
             made with the REST API.",
            json!({
                "conversation_id": id_property("Where the page lives."),
                "title": { "type": "string", "description": "Empty means \"Untitled\"." },
                "body": { "type": "string", "description": "Markdown." },
                "idempotency_key": key_property(),
            }),
            &["conversation_id"],
            false,
        ),
        tool(
            "update_page",
            PagesWrite,
            "Edit a page",
            "Change a page's title or text. base_revision is the revision you last read; if the \
             page has changed since, the call fails with stale_revision and you should fetch it \
             again. Give body to replace the text or append to add after it.",
            json!({
                "page_id": id_property("The page."),
                "base_revision": { "type": "integer", "minimum": 1 },
                "title": { "type": "string" },
                "body": { "type": "string", "description": "Replaces the text." },
                "append": { "type": "string", "description": "Added after the text." },
                "idempotency_key": key_property(),
            }),
            &["page_id", "base_revision"],
            false,
        ),
        tool(
            "trash",
            PagesWrite,
            "Move a page to the trash",
            "Trash a page or board. It can be restored for 30 days.",
            json!({
                "page_id": id_property("The page."),
                "idempotency_key": key_property(),
            }),
            &["page_id"],
            false,
        ),
        tool(
            "list_conversations",
            ConversationsRead,
            "List conversations",
            "The conversations the token's owner is a member of, in one workspace or all of them.",
            json!({ "workspace_id": id_property("Limit to one workspace.") }),
            &[],
            true,
        ),
        tool(
            "list_notations",
            NotationsRead,
            "List notations",
            "Notes anchored to text on pages. Give page_id for one page's notations; otherwise \
             those assigned to (or written by) the token's owner.",
            json!({
                "page_id": id_property("One page's notations."),
                "workspace_id": id_property("Limit to one workspace."),
                "scope": { "enum": ["assigned", "created"], "description": "Default assigned." },
                "completed": { "type": "boolean", "description": "Show completed ones." },
                "q": { "type": "string" },
            }),
            &[],
            true,
        ),
        tool(
            "add_notation",
            NotationsWrite,
            "Add a notation",
            "Leave a note on text of a page. quote is the text, from and to are its character \
             range in the page's current text.",
            json!({
                "page_id": id_property("The page (not a board)."),
                "quote": { "type": "string" },
                "from": { "type": "integer", "minimum": 0 },
                "to": { "type": "integer", "minimum": 1 },
                "body": { "type": "string" },
                "assignee_id": id_property("Who it is for; defaults to the token's owner."),
                "idempotency_key": key_property(),
            }),
            &["page_id", "quote", "from", "to", "body"],
            false,
        ),
        tool(
            "complete_notation",
            NotationsWrite,
            "Complete a notation",
            "Mark a notation done, or reopen it with completed false.",
            json!({
                "notation_id": id_property("The notation."),
                "completed": { "type": "boolean", "description": "Default true." },
                "idempotency_key": key_property(),
            }),
            &["notation_id"],
            false,
        ),
        tool(
            "list_versions",
            PagesRead,
            "List a page's versions",
            "Earlier states of a page, newest first.",
            json!({ "page_id": id_property("The page.") }),
            &["page_id"],
            true,
        ),
        tool(
            "restore_version",
            PagesWrite,
            "Restore a version",
            "Make an earlier version the page's content. The current state is kept as a version \
             first, so this can be undone.",
            json!({
                "page_id": id_property("The page."),
                "version": { "type": "integer", "minimum": 1 },
                "base_revision": { "type": "integer", "minimum": 1 },
                "idempotency_key": key_property(),
            }),
            &["page_id", "version", "base_revision"],
            false,
        ),
    ]
}

/// A tool's failure: either something the model can fix (a tool result with `isError`), or a
/// fault of the server's (a JSON-RPC error).
enum ToolFailure {
    Tool(String),
    Server,
}

impl From<AppError> for ToolFailure {
    fn from(error: AppError) -> Self {
        match error {
            AppError::Api(error) => {
                let code = serde_json::to_value(error.code)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_default();
                Self::Tool(format!("{code}: {}", error.message))
            }
            // The kind of failure, never its text (a database error can quote a value).
            other => {
                tracing::error!(error = %other.describe(), "a tool call failed");
                Self::Server
            }
        }
    }
}

fn bad_argument(message: impl Into<String>) -> ToolFailure {
    ToolFailure::Tool(format!("invalid_request: {}", message.into()))
}

/// The arguments of a call, read by name.
struct Args<'a>(&'a serde_json::Map<String, Value>);

impl Args<'_> {
    fn required<T: std::str::FromStr>(&self, name: &str) -> Result<T, ToolFailure> {
        self.optional(name)?
            .ok_or_else(|| bad_argument(format!("{name} is required")))
    }

    fn optional<T: std::str::FromStr>(&self, name: &str) -> Result<Option<T>, ToolFailure> {
        match self.0.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => text
                .parse()
                .map(Some)
                .map_err(|_| bad_argument(format!("{name} is not valid"))),
            Some(other) => other
                .to_string()
                .parse()
                .map(Some)
                .map_err(|_| bad_argument(format!("{name} is not valid"))),
        }
    }

    fn text(&self, name: &str) -> Result<Option<String>, ToolFailure> {
        match self.0.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => Ok(Some(text.clone())),
            Some(_) => Err(bad_argument(format!("{name} must be text"))),
        }
    }

    fn flag(&self, name: &str) -> Result<Option<bool>, ToolFailure> {
        match self.0.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Bool(flag)) => Ok(Some(*flag)),
            Some(_) => Err(bad_argument(format!("{name} must be true or false"))),
        }
    }
}

/// An operation id that is the same for the same (person, tool, key, purpose) and fresh when no key
/// was given, so a model that repeats a call with its key does not repeat the write.
fn derived(user: UserId, tool: &str, key: &Option<String>, purpose: &str) -> Uuid {
    match key {
        Some(key) => {
            let name = format!("{user}\u{0}{tool}\u{0}{key}\u{0}{purpose}");
            Uuid::new_v5(&Uuid::NAMESPACE_OID, name.as_bytes())
        }
        None => Uuid::now_v7(),
    }
}

fn operation(user: UserId, tool: &str, key: &Option<String>) -> OperationId {
    OperationId::from_uuid(derived(user, tool, key, "operation"))
}

async fn call_tool(
    state: &AppState,
    caller: &Caller,
    id: Value,
    params: Value,
    modern: bool,
) -> Response {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return rpc_error(
            StatusCode::OK,
            id,
            INVALID_PARAMS,
            "tools/call needs a tool name",
            None,
        );
    };
    let Some(tool) = tool_definitions()
        .into_iter()
        .find(|tool| tool.name == name)
    else {
        return rpc_error(
            StatusCode::OK,
            id,
            INVALID_PARAMS,
            &format!("unknown tool: {name}"),
            None,
        );
    };
    let empty = serde_json::Map::new();
    let arguments = match params.get("arguments") {
        None | Some(Value::Null) => &empty,
        Some(Value::Object(map)) => map,
        Some(_) => {
            return rpc_error(
                StatusCode::OK,
                id,
                INVALID_PARAMS,
                "arguments must be an object",
                None,
            );
        }
    };

    let outcome = if caller.allows(tool.scope) {
        run_tool(state, caller.user, name, Args(arguments)).await
    } else {
        Err(ToolFailure::Tool(format!(
            "forbidden: this token does not have the {} scope",
            tool.scope.as_str()
        )))
    };
    let mut result = match outcome {
        Ok(value) => {
            // Structured content is an object in every revision that has it; a list is wrapped.
            let structured = if value.is_object() {
                value
            } else {
                json!({ "items": value })
            };
            json!({
                "content": [{ "type": "text", "text": serde_json::to_string(&structured).unwrap_or_default() }],
                "structuredContent": structured,
                "isError": false,
            })
        }
        Err(ToolFailure::Tool(message)) => json!({
            "content": [{ "type": "text", "text": message }],
            "isError": true,
        }),
        Err(ToolFailure::Server) => {
            return rpc_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                id,
                INTERNAL_ERROR,
                "internal server error",
                None,
            );
        }
    };
    if modern {
        result["resultType"] = json!("complete");
    }
    rpc_result(id, result)
}

/// The workspaces the person is an active member of, oldest first.
async fn workspaces_of(state: &AppState, user: UserId) -> Result<Vec<WorkspaceId>, AppError> {
    let ids: Vec<Uuid> = sqlx::query_scalar(concat!(
        "SELECT wm.workspace_id FROM workspace_members wm \
         JOIN workspaces w ON w.id = wm.workspace_id \
         WHERE wm.user_id = $1 AND ",
        active_member!("wm"),
        " ORDER BY w.created_at, w.id"
    ))
    .bind(user.as_uuid())
    .fetch_all(&state.pool)
    .await?;
    Ok(ids.into_iter().map(WorkspaceId::from_uuid).collect())
}

/// The one workspace asked for, or all of the person's.
async fn target_workspaces(
    state: &AppState,
    user: UserId,
    asked: Option<WorkspaceId>,
) -> Result<Vec<WorkspaceId>, AppError> {
    match asked {
        Some(workspace) => Ok(vec![workspace]),
        None => workspaces_of(state, user).await,
    }
}

fn to_json<T: serde::Serialize>(value: &T) -> Result<Value, ToolFailure> {
    serde_json::to_value(value).map_err(|_| ToolFailure::Server)
}

async fn run_tool(
    state: &AppState,
    user: UserId,
    name: &str,
    args: Args<'_>,
) -> Result<Value, ToolFailure> {
    let pool = &state.pool;
    let key = args.text("idempotency_key")?;
    match name {
        "search" => {
            let query: String = args.required("query")?;
            let kind = match args.text("kind")?.as_deref() {
                None => None,
                Some("page") => Some(PageKind::Page),
                Some("board") => Some(PageKind::Board),
                Some(_) => return Err(bad_argument("kind is page or board")),
            };
            let limit = args
                .optional::<usize>("limit")?
                .unwrap_or(DEFAULT_SEARCH_RESULTS)
                .clamp(1, MAX_SEARCH_RESULTS);
            let mut found = Vec::new();
            for workspace in target_workspaces(state, user, args.optional("workspace_id")?).await? {
                let params = PageListParams {
                    q: Some(query.clone()),
                    kind,
                    limit: Some(limit as u32),
                    ..PageListParams::default()
                };
                found.extend(
                    pages::list_workspace(pool, user, workspace, params)
                        .await?
                        .pages,
                );
            }
            found.truncate(limit);
            to_json(&json!({ "pages": found }))
        }
        "fetch" => {
            let page = pages::get(pool, user, args.required::<PageId>("page_id")?).await?;
            to_json(&PageResponse { page })
        }
        "create_page" => {
            let conversation: ConversationId = args.required("conversation_id")?;
            let request = CreatePageRequest {
                operation_id: operation(user, name, &key),
                page_id: PageId::from_uuid(derived(user, name, &key, "page")),
                kind: PageKind::Page,
                title: args.text("title")?.unwrap_or_default(),
                content: Some(PageContent::Page {
                    body: args.text("body")?.unwrap_or_default(),
                }),
            };
            let page = pages::create(pool, user, conversation, request).await?;
            to_json(&PageResponse { page })
        }
        "update_page" => {
            let page: PageId = args.required("page_id")?;
            let body = args.text("body")?;
            let append = args.text("append")?;
            if body.is_some() && append.is_some() {
                return Err(bad_argument("give body or append, not both"));
            }
            let request = UpdatePageRequest {
                operation_id: operation(user, name, &key),
                base_revision: args.required("base_revision")?,
                title: args.text("title")?,
                content: body.map(|body| PageContent::Page { body }),
                append,
                conversation_id: None,
            };
            let page = pages::update(pool, user, page, request).await?;
            to_json(&PageResponse { page })
        }
        "trash" => {
            let page: PageId = args.required("page_id")?;
            let page = pages::trash(pool, user, page, operation(user, name, &key)).await?;
            to_json(&PageResponse { page })
        }
        "list_conversations" => {
            let mut all = Vec::new();
            for workspace in target_workspaces(state, user, args.optional("workspace_id")?).await? {
                all.extend(
                    conversations::list_member(pool, user, workspace)
                        .await?
                        .conversations,
                );
            }
            to_json(&json!({ "conversations": all }))
        }
        "list_notations" => {
            if let Some(page) = args.optional::<PageId>("page_id")? {
                return to_json(&notations::list_page(pool, user, page).await?);
            }
            let scope = match args.text("scope")?.as_deref() {
                None | Some("assigned") => NotationScope::Assigned,
                Some("created") => NotationScope::Created,
                Some(_) => return Err(bad_argument("scope is assigned or created")),
            };
            let mut all = Vec::new();
            for workspace in target_workspaces(state, user, args.optional("workspace_id")?).await? {
                let params = NotationListParams {
                    scope,
                    completed: args.flag("completed")?.unwrap_or(false),
                    q: args.text("q")?,
                };
                all.extend(
                    notations::list_workspace(pool, user, workspace, params)
                        .await?
                        .notations,
                );
            }
            to_json(&json!({ "notations": all }))
        }
        "add_notation" => {
            let page: PageId = args.required("page_id")?;
            let request = CreateNotationRequest {
                operation_id: operation(user, name, &key),
                notation_id: NotationId::from_uuid(derived(user, name, &key, "notation")),
                quote: args.required("quote")?,
                from: args.required("from")?,
                to: args.required("to")?,
                body: args.required("body")?,
                assignee_id: args.optional("assignee_id")?,
            };
            let notation = notations::create(pool, user, page, request).await?;
            to_json(&NotationResponse { notation })
        }
        "complete_notation" => {
            let notation: NotationId = args.required("notation_id")?;
            let request = SetNotationCompletedRequest {
                operation_id: operation(user, name, &key),
                completed: args.flag("completed")?.unwrap_or(true),
            };
            let notation = notations::set_completed(pool, user, notation, request).await?;
            to_json(&NotationResponse { notation })
        }
        "list_versions" => {
            let page: PageId = args.required("page_id")?;
            to_json(&pages::versions(pool, user, page).await?)
        }
        "restore_version" => {
            let page: PageId = args.required("page_id")?;
            let request = RestoreVersionRequest {
                operation_id: operation(user, name, &key),
                base_revision: args.required("base_revision")?,
            };
            let page = pages::restore_version(pool, user, page, args.required("version")?, request)
                .await?;
            to_json(&PageResponse { page })
        }
        _ => Err(ToolFailure::Tool(format!(
            "{}: unknown tool",
            serde_json::to_value(ErrorCode::NotFound)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_decodes_the_sentinel_form_of_a_name() {
        assert_eq!(
            base64_decode("SGVsbG8sIOS4lueVjA==").unwrap(),
            "Hello, 世界".as_bytes()
        );
        assert_eq!(base64_decode("Zm9v").unwrap(), b"foo");
        assert!(base64_decode("not base64!").is_none());
        let value = HeaderValue::from_static("=?base64?Zm9v?=");
        assert_eq!(header_text(&value).as_deref(), Some("foo"));
        let plain = HeaderValue::from_static("search");
        assert_eq!(header_text(&plain).as_deref(), Some("search"));
    }

    #[test]
    fn origin_must_be_this_host_when_present() {
        let mut headers = HeaderMap::new();
        assert!(origin_is_acceptable(&headers));
        headers.insert(HOST, HeaderValue::from_static("lazy.example.com"));
        headers.insert(ORIGIN, HeaderValue::from_static("https://lazy.example.com"));
        assert!(origin_is_acceptable(&headers));
        headers.insert(ORIGIN, HeaderValue::from_static("https://evil.example.com"));
        assert!(!origin_is_acceptable(&headers));
        headers.insert(ORIGIN, HeaderValue::from_static("null"));
        assert!(!origin_is_acceptable(&headers));
    }

    #[test]
    fn tool_names_are_unique_and_valid_and_schemas_are_objects() {
        let tools = tool_definitions();
        let mut names = std::collections::HashSet::new();
        for tool in &tools {
            assert!(names.insert(tool.name));
            assert!(
                tool.name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
            );
            assert_eq!(tool.definition["inputSchema"]["type"], "object");
            assert_eq!(tool.definition["name"], tool.name);
        }
        assert_eq!(tools.len(), 11);
    }

    #[test]
    fn a_key_makes_the_same_ids_and_no_key_makes_new_ones() {
        let user = UserId::generate();
        let key = Some("k".to_owned());
        assert_eq!(
            derived(user, "create_page", &key, "page"),
            derived(user, "create_page", &key, "page")
        );
        assert_ne!(
            derived(user, "create_page", &key, "page"),
            derived(user, "create_page", &key, "operation")
        );
        assert_ne!(
            derived(user, "create_page", &key, "page"),
            derived(user, "trash", &key, "page")
        );
        assert_ne!(
            derived(user, "trash", &None, "operation"),
            derived(user, "trash", &None, "operation")
        );
        assert_ne!(
            derived(UserId::generate(), "trash", &key, "operation"),
            derived(user, "trash", &key, "operation")
        );
    }
}
