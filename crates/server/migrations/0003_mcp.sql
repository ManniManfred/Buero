-- Sammlungen (samt Untersammlungen), deren Dokumente der MCP-Zugang (KI) nie ausliefert
ALTER TABLE collections ADD COLUMN ai_locked INTEGER NOT NULL DEFAULT 0;

-- Zugangsschlüssel für den MCP-Endpunkt (/mcp); gilt nur dort, nicht für die übrige API
CREATE TABLE api_tokens (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    name         TEXT    NOT NULL,
    token_hash   TEXT    NOT NULL UNIQUE,
    created_at   INTEGER NOT NULL,
    last_used_at INTEGER
);
