-- Personal access tokens (llm-docs/DECISIONS.md, D31): a credential for scripts and MCP clients
-- that is not a person's session. Only the SHA-256 of the secret is stored; the secret is shown
-- once, when the token is made. A token reaches only the routes that name one of its scopes.
CREATE TABLE access_tokens (
    id           uuid PRIMARY KEY,
    user_id      uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name         text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 64),
    token_hash   bytea NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
    -- The wire names of the scopes ("pages:read"), at least one, no repeats.
    scopes       text[] NOT NULL CHECK (cardinality(scopes) >= 1),
    created_at   timestamptz NOT NULL DEFAULT now(),
    -- Written at most once a minute per token, so a busy script does not write on every request.
    last_used_at timestamptz,
    expires_at   timestamptz,
    revoked_at   timestamptz
);

-- The list and the per-person limit read a person's live tokens.
CREATE INDEX access_tokens_user_idx ON access_tokens (user_id, created_at, id)
    WHERE revoked_at IS NULL;
