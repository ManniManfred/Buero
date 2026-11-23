-- Unterdokumente (z. B. Mail-Anhänge): genau ein übergeordnetes Dokument
ALTER TABLE documents ADD COLUMN parent_id INTEGER REFERENCES documents(id) ON DELETE SET NULL;
CREATE INDEX idx_documents_parent ON documents(parent_id);

-- Verweise zwischen Dokumenten (gegenseitig, a_id < b_id)
CREATE TABLE document_links (
    a_id       INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    b_id       INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (a_id, b_id),
    CHECK (a_id < b_id)
);
CREATE INDEX idx_document_links_b ON document_links(b_id);

-- Bisherige Mail-Anhänge werden Unterdokumente ihrer ersten Mail …
UPDATE documents
   SET parent_id = (SELECT MIN(ea.email_id) FROM email_attachments ea WHERE ea.document_id = documents.id)
 WHERE parent_id IS NULL AND id IN (SELECT document_id FROM email_attachments);
-- … weitere Mails mit demselben Anhang verweisen darauf
INSERT OR IGNORE INTO document_links (a_id, b_id, created_at)
SELECT MIN(ea.email_id, ea.document_id), MAX(ea.email_id, ea.document_id), CAST(strftime('%s', 'now') AS INTEGER)
  FROM email_attachments ea
 WHERE ea.email_id <> ea.document_id
   AND ea.email_id <> IFNULL((SELECT d.parent_id FROM documents d WHERE d.id = ea.document_id), 0);
