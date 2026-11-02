# Docs: pages, boards, and notations

Lazy's docs are a port of the docs features of Corpus, a self-hosted writing and diagramming app. The decisions are in [DECISIONS.md](DECISIONS.md) (D30 for the docs, D31 for tokens, the API description, and MCP). The server is described in [SERVER.md](SERVER.md), the client in [CLIENT.md](CLIENT.md). This file is the overview.

## What you can do

- **Pages.** Write markdown under a title that is the page's first line. There is no separate preview: a page is one document. Pages autosave, and the bar above a page says only when a save is pending, failing, or in conflict.
- **Boards.** Draw shapes (rectangle, ellipse, diamond, note, text), join them with connectors, color them, and move around the canvas with pan and zoom.
- **Pages and boards are equals.** Neither is nested in the other. Both live in a conversation (a channel, direct message, or group). **Docs** is in the sidebar, under People (`Cmd+Shift+D`, or `Ctrl+Shift+D`), and lists every page and board you can read; the conversation chip narrows the list to one conversation. **New** (page or board) goes to the conversation the chip names, else the one you had open, else the default channel. A page's bar has the way back, the conversation it lives in (click to move it), Notes, History, and a **…** menu with Add note (`Cmd+Alt+M`, or `Ctrl+Alt+M`), Attach a file, Move, and Move to trash.
- **History.** Every page keeps up to 100 earlier states. Preview one, restore it, and undo the restore: a restore first keeps the page as it was.
- **Trash.** Deleting moves a page to the conversation's trash for 30 days. Restore it, or delete it for good.
- **Notations.** Select text on a page and leave a note for yourself or a teammate. Notes have replies, an assignee, and a done state. *Notes for me* (`Cmd+Shift+N`, or `Ctrl+Shift+N`) lists what is assigned to you, or what you wrote.
- **Files.** Attach files to a page. They follow the page's access and go with it when the page is purged.
- **Search.** Find pages and boards by title and text across the workspace (or the one conversation the chip names).
- **Scripts and agents.** Personal access tokens, an OpenAPI description, and an MCP server let a script or an AI client search, read, and edit pages.

## Who can see and change a page

A page uses the access of its conversation. There are no separate page permissions.

| | Read, list, history, files | Create, edit, move, trash, restore, purge, add files, make notations and replies |
| --- | --- | --- |
| Member of a private channel | yes | yes |
| Member of a public channel | yes | yes |
| Workspace member, not in the public channel | yes | no |
| Anyone, in an archived channel | yes | no |
| Not in a private channel | no (the page does not exist for them) | no |

Leaving a channel removes its pages from view immediately. Owners and admins have no extra access to private channels.

## Editing and conflicts

Each page has a `revision`. An edit says which revision it started from. If someone else changed the page first, the edit is refused with `stale_revision` and the app shows both versions: **Keep mine** saves your text over theirs as a new revision, **Take theirs** drops your edit. Nothing merges, and two people typing in one page at once is not supported. Version history keeps the state you overwrote.

## Moving around

A page moves to another conversation of the same workspace if you can post in both. Its notations, files, and history move with it.

## API summary

Everything the app does goes through the same `/v1` routes a token can reach. Paths are in `crates/lazy-protocol/src/docs.rs` (`doc_paths`).

| Route | Purpose |
| --- | --- |
| `POST /v1/conversations/{id}/pages`, `GET` | Create, list a conversation's pages |
| `GET /v1/workspaces/{id}/pages` | List and search across a workspace (`q`, `kind`, `state`, `sort`, `limit`, `offset`) |
| `GET`, `PATCH`, `DELETE /v1/pages/{id}` | Read, edit or move, trash |
| `POST /v1/pages/{id}/restore`, `/purge` | Leave the trash, delete for good |
| `GET /v1/pages/{id}/versions`, `.../{n}`, `POST .../{n}/restore` | History |
| `PUT /v1/pages/{id}/files/{attachment_id}` | Add a file (download with `GET /v1/attachments/{id}`) |
| `GET`, `POST /v1/pages/{id}/notations` | A page's notations |
| `GET /v1/workspaces/{id}/notations` | Assigned to you, or written by you |
| `PATCH`, `DELETE /v1/notations/{id}`, `PUT .../completed`, `POST .../replies` | Work with one notation |
| `POST`, `GET /v1/tokens`, `DELETE /v1/tokens/{id}` | Personal access tokens |
| `GET /v1/workspaces/{id}/conversations` | The conversations you are in (where pages live) |
| `GET /v1/openapi.json` | OpenAPI 3.1 description |
| `POST /mcp` | MCP server |

Every mutation takes an `operation_id`. Repeating a request with the same id and body returns the original result; the same id with a different body is `conflict`.

## Tokens

The app has no screen for tokens yet. Make one with the API while signed in with a session (`POST /v1/tokens` with your password, a name, and the scopes you want); the secret in the answer is shown once. A token has the access of its owner, limited to its scopes, and reaches only the docs routes, `GET /v1/me`, and conversation lists. It cannot send messages or change a workspace.

```sh
curl -H "Authorization: Bearer $LAZY_TOKEN" -H "Accept: application/json" \
  "https://lazy.example.com/v1/workspaces/$WORKSPACE/pages?q=roadmap"
```

## MCP

Point an MCP client at `https://lazy.example.com/mcp` with the header `Authorization: Bearer <token>`; the token needs the `mcp:use` scope plus the scopes for what the tools do. Lazy does not offer OAuth (see D31), so a client that can only authenticate with OAuth cannot connect.

The endpoint speaks MCP revision 2026-07-28 and, on the same URL, 2025-11-25, 2025-06-18, and 2025-03-26 (clients of those revisions start with `initialize`). It answers each request with one JSON object, offers no event stream or sessions (`GET` is `405`), and offers only tools. A tool the token's scopes do not allow is not listed. Every write tool takes an optional `idempotency_key`: calling again with the same key does not write again. A tool that fails because of something the caller can fix (a stale `base_revision`, a page it cannot see) returns an error result, not a protocol error. Details are in [SERVER.md](SERVER.md).

| Tool | Purpose |
| --- | --- |
| `search`, `fetch` | Find and read pages |
| `create_page`, `update_page` | Write pages. `update_page` takes `base_revision` and refuses stale writes |
| `trash` | Move a page to the trash |
| `list_conversations` | Conversations the token's owner can read |
| `list_notations`, `add_notation`, `complete_notation` | Work with notations |
| `list_versions`, `restore_version` | Read history and roll back |

## Limits

| What | Limit |
| --- | --- |
| Title | 255 characters |
| Page text | 1,000,000 bytes |
| Board | 500 shapes, 1,000 connectors, 2,000 bytes of text per shape |
| Pages and boards per conversation | 1,000 live |
| Versions per page | 100; one automatic snapshot per 5 minutes |
| Trash retention | 30 days |
| Notation quote / body | 2,000 / 5,000 bytes |
| Tokens per person | 25 |

## Differences from Corpus

Corpus has spaces with their own roles and visibility, a personal space per user, an audit log, user administration, and OAuth for MCP. Lazy has conversations, workspace roles, an event log, and workspace administration, so pages use those and the rest was not ported. See D30 and D31 for the reasons.

## Status

See the tasks 3.10 and 3.11 in [IMPLEMENTATION_PLAN.md](IMPLEMENTATION_PLAN.md) and the "Docs" section of [VERIFICATION.md](VERIFICATION.md) for what has been built and what has been measured, observed, or not checked.
