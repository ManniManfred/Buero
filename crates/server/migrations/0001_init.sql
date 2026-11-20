CREATE TABLE documents (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    title             TEXT    NOT NULL,
    description       TEXT    NOT NULL DEFAULT '',
    original_filename TEXT    NOT NULL,
    mime              TEXT    NOT NULL,
    extension         TEXT    NOT NULL DEFAULT '',
    size              INTEGER NOT NULL,
    sha256            TEXT    NOT NULL,
    version           INTEGER NOT NULL DEFAULT 1,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL,
    document_date     TEXT,
    extract_status    TEXT    NOT NULL DEFAULT 'pending',
    extract_error     TEXT,
    page_count        INTEGER,
    has_thumbnail     INTEGER NOT NULL DEFAULT 0,
    content           TEXT    NOT NULL DEFAULT ''
);
CREATE INDEX idx_documents_sha ON documents(sha256);
CREATE INDEX idx_documents_status ON documents(extract_status);

CREATE TABLE document_versions (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    document_id       INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    version           INTEGER NOT NULL,
    original_filename TEXT    NOT NULL,
    size              INTEGER NOT NULL,
    sha256            TEXT    NOT NULL,
    created_at        INTEGER NOT NULL,
    UNIQUE(document_id, version)
);
CREATE INDEX idx_versions_sha ON document_versions(sha256);

-- Hierarchische Sammlungen. Ein Dokument kann in beliebig vielen Sammlungen liegen.
CREATE TABLE collections (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    parent_id  INTEGER REFERENCES collections(id) ON DELETE CASCADE,
    name       TEXT    NOT NULL,
    color      TEXT,
    created_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX idx_collections_name ON collections(IFNULL(parent_id, 0), name);

CREATE TABLE document_collections (
    document_id   INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    PRIMARY KEY (document_id, collection_id)
);
CREATE INDEX idx_dc_collection ON document_collections(collection_id);

CREATE TABLE tags (
    id    INTEGER PRIMARY KEY AUTOINCREMENT,
    name  TEXT NOT NULL UNIQUE COLLATE NOCASE,
    color TEXT
);

CREATE TABLE document_tags (
    document_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    tag_id      INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (document_id, tag_id)
);
CREATE INDEX idx_dt_tag ON document_tags(tag_id);

CREATE TABLE sessions (
    token_hash TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);
