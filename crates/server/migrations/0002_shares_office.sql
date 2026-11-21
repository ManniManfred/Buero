-- Freigabelinks für einzelne Dokumente oder ganze Sammlungen (inkl. Untersammlungen)
CREATE TABLE shares (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    token          TEXT    NOT NULL UNIQUE,
    document_id    INTEGER REFERENCES documents(id) ON DELETE CASCADE,
    collection_id  INTEGER REFERENCES collections(id) ON DELETE CASCADE,
    -- "salt$sha256(salt + passwort)" oder NULL
    password_hash  TEXT,
    allow_upload   INTEGER NOT NULL DEFAULT 0,
    expires_at     INTEGER,
    note           TEXT    NOT NULL DEFAULT '',
    created_at     INTEGER NOT NULL,
    access_count   INTEGER NOT NULL DEFAULT 0,
    last_access_at INTEGER,
    CHECK ((document_id IS NULL) <> (collection_id IS NULL))
);
CREATE INDEX idx_shares_document ON shares(document_id);
CREATE INDEX idx_shares_collection ON shares(collection_id);

-- Schlüssel der laufenden Euro-Office-Bearbeitung (bleibt bis zum Schließen gleich)
ALTER TABLE documents ADD COLUMN office_key TEXT;
