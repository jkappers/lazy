-- Docs (protocol version 15; llm-docs/DECISIONS.md D30): pages and boards that belong to a
-- conversation, their versions, notations with replies, and files on pages. Additive: existing
-- databases upgrade in place and every conversation simply has no pages.

-- A page (markdown text) or a board (shapes and connectors). Who may read or write follows the
-- conversation. A trashed page keeps its row until it is purged for good.
CREATE TABLE pages (
    -- Chosen by the creating client, so a retried creation is the same creation.
    id              uuid PRIMARY KEY,
    workspace_id    uuid        NOT NULL REFERENCES workspaces (id) ON DELETE CASCADE,
    conversation_id uuid        NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
    kind            text        NOT NULL CHECK (kind IN ('page', 'board')),
    title           text        NOT NULL CHECK (char_length(title) BETWEEN 1 AND 255),
    -- Markdown; present exactly for a page.
    body            text,
    -- The lazy_domain::Board value; present exactly for a board.
    board           jsonb,
    -- The first 160 characters of the text on one line, kept up to date by every write so a list
    -- never reads a body.
    excerpt         text        NOT NULL DEFAULT '',
    -- Starts at 1; rises with every change to the title, the content, or the conversation.
    revision        integer     NOT NULL DEFAULT 1 CHECK (revision >= 1),
    -- The number of the newest version the page has had. Version numbers are never reused, even
    -- after old versions are removed.
    version_head    integer     NOT NULL DEFAULT 0 CHECK (version_head >= 0),
    created_by      uuid        NOT NULL REFERENCES users (id),
    created_at      timestamptz NOT NULL,
    updated_by      uuid        NOT NULL REFERENCES users (id),
    updated_at      timestamptz NOT NULL,
    -- Set while the page is in the trash.
    trashed_at      timestamptz,
    CHECK ((kind = 'page' AND body IS NOT NULL AND board IS NULL)
           OR (kind = 'board' AND board IS NOT NULL AND body IS NULL)),
    -- Title and text, 'simple' configuration like messages. Only the first 200,000 characters of
    -- a page's text are indexed: a tsvector holds at most 1 MiB.
    search_vector   tsvector GENERATED ALWAYS AS (
        to_tsvector(
            'simple',
            title || ' ' || COALESCE(
                left(body, 200000),
                COALESCE(jsonb_path_query_array(board, '$.nodes[*].text')::text, '') || ' ' ||
                COALESCE(jsonb_path_query_array(board, '$.edges[*].label')::text, ''),
                ''
            )
        )
    ) STORED
);

-- A conversation's live pages, newest change first (the list), and its trash.
CREATE INDEX pages_live_idx ON pages (conversation_id, updated_at DESC, id DESC)
    WHERE trashed_at IS NULL;
CREATE INDEX pages_trash_idx ON pages (conversation_id, trashed_at DESC, id DESC)
    WHERE trashed_at IS NOT NULL;
-- The retention job: trash past its time.
CREATE INDEX pages_purge_idx ON pages (trashed_at) WHERE trashed_at IS NOT NULL;
CREATE INDEX pages_workspace_idx ON pages (workspace_id, updated_at DESC, id DESC);
CREATE INDEX pages_search_idx ON pages USING gin (search_vector);

-- The earlier states of a page. A version is the state *before* a change, so the page's current
-- state is never in this table.
CREATE TABLE page_versions (
    page_id    uuid        NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
    version    integer     NOT NULL CHECK (version >= 1),
    title      text        NOT NULL,
    body       text,
    board      jsonb,
    -- 'autosave' or 'pre_restore'.
    reason     text        NOT NULL CHECK (reason IN ('autosave', 'pre_restore')),
    -- Who made the change this version preceded.
    created_by uuid        REFERENCES users (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    -- The page's revision when this state was current.
    revision   integer     NOT NULL,
    PRIMARY KEY (page_id, version),
    CHECK ((body IS NULL) <> (board IS NULL))
);

-- A note anchored to text on a page, assigned to someone.
CREATE TABLE notations (
    id           uuid PRIMARY KEY,
    page_id      uuid        NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
    author_id    uuid        NOT NULL REFERENCES users (id),
    assignee_id  uuid        NOT NULL REFERENCES users (id),
    quote        text        NOT NULL,
    -- Character offsets in the page's text when the note was made.
    from_pos     integer     NOT NULL CHECK (from_pos >= 0),
    to_pos       integer     NOT NULL,
    body         text        NOT NULL,
    created_at   timestamptz NOT NULL,
    updated_at   timestamptz NOT NULL,
    completed_at timestamptz,
    resolved_by  uuid        REFERENCES users (id),
    CHECK (to_pos > from_pos),
    CHECK ((completed_at IS NULL) = (resolved_by IS NULL))
);
CREATE INDEX notations_page_idx ON notations (page_id, created_at, id);
CREATE INDEX notations_assignee_idx ON notations (assignee_id, completed_at);
CREATE INDEX notations_author_idx ON notations (author_id, completed_at);
-- The count of open notations on a page.
CREATE INDEX notations_open_idx ON notations (page_id) WHERE completed_at IS NULL;

CREATE TABLE notation_replies (
    id          uuid PRIMARY KEY,
    notation_id uuid        NOT NULL REFERENCES notations (id) ON DELETE CASCADE,
    author_id   uuid        NOT NULL REFERENCES users (id),
    body        text        NOT NULL,
    created_at  timestamptz NOT NULL
);
CREATE INDEX notation_replies_idx ON notation_replies (notation_id, created_at, id);

-- Files on pages. A page file is attached from the moment it is uploaded: it is never pending,
-- never expires, and is removed with its page. The row names the page; `conversation_id` follows
-- the page when it moves. SET NULL, not CASCADE: a purge marks the rows deleted first, and the
-- cleanup job removes the bytes and then the rows.
ALTER TABLE attachments ADD COLUMN page_id uuid REFERENCES pages (id) ON DELETE SET NULL;
ALTER TABLE attachments ADD CONSTRAINT attachments_one_owner CHECK (page_id IS NULL OR message_id IS NULL);
CREATE INDEX attachments_page_idx ON attachments (page_id, created_at, id)
    WHERE page_id IS NOT NULL AND deleted_at IS NULL;

-- Receipts also cover the docs commands. `target_id` is the page or notation the command is about;
-- it is not a foreign key because the receipt outlives a purged page (a repeated create must not
-- make the page again).
ALTER TABLE operation_receipts ADD COLUMN target_id uuid;

ALTER TABLE operation_receipts DROP CONSTRAINT operation_receipts_result_shape;
ALTER TABLE operation_receipts
    ADD CONSTRAINT operation_receipts_result_shape CHECK (
        (kind IN ('send_message', 'edit_message')
            AND message_id IS NOT NULL AND conversation_id IS NULL AND group_id IS NULL
            AND target_id IS NULL)
        OR (kind IN ('create_channel', 'open_conversation')
            AND conversation_id IS NOT NULL AND message_id IS NULL AND group_id IS NULL
            AND target_id IS NULL)
        OR (kind = 'create_group'
            AND group_id IS NOT NULL AND message_id IS NULL AND conversation_id IS NULL
            AND target_id IS NULL)
        OR (kind IN ('create_page', 'change_page', 'create_notation', 'change_notation')
            AND target_id IS NOT NULL AND message_id IS NULL AND conversation_id IS NULL
            AND group_id IS NULL)
    );
