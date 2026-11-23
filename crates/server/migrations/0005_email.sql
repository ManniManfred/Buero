-- E-Mails (.eml): Kopfdaten für Konversationen, Anhänge als eigene Dokumente
CREATE TABLE emails (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    document_id  INTEGER NOT NULL UNIQUE REFERENCES documents(id) ON DELETE CASCADE,
    message_id   TEXT,
    -- In-Reply-To und References, je als „<id>“ durch Leerzeichen getrennt
    refs         TEXT    NOT NULL DEFAULT '',
    thread_id    TEXT    NOT NULL,
    sender       TEXT    NOT NULL DEFAULT '',
    recipients   TEXT    NOT NULL DEFAULT '',
    cc           TEXT    NOT NULL DEFAULT '',
    subject      TEXT    NOT NULL DEFAULT '',
    sent_at      INTEGER
);
CREATE INDEX idx_emails_message ON emails(message_id);
CREATE INDEX idx_emails_thread ON emails(thread_id);

CREATE TABLE email_attachments (
    email_id     INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    document_id  INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    PRIMARY KEY (email_id, document_id)
);
CREATE INDEX idx_email_attachments_doc ON email_attachments(document_id);

-- IMAP-Postfächer, aus denen Büro Mails abruft
CREATE TABLE mail_accounts (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    name          TEXT    NOT NULL,
    host          TEXT    NOT NULL,
    port          INTEGER NOT NULL DEFAULT 993,
    username      TEXT    NOT NULL,
    password      TEXT    NOT NULL DEFAULT '',
    enabled       INTEGER NOT NULL DEFAULT 1,
    interval_min  INTEGER NOT NULL DEFAULT 15,
    collection_id INTEGER REFERENCES collections(id) ON DELETE SET NULL,
    last_run      INTEGER,
    last_error    TEXT,
    last_imported INTEGER NOT NULL DEFAULT 0
);

-- Ordner eines Postfachs; mode: 'all' = alle Mails, 'replies' = nur Mails zu Konversationen, die schon in Büro sind
CREATE TABLE mail_folders (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id    INTEGER NOT NULL REFERENCES mail_accounts(id) ON DELETE CASCADE,
    name          TEXT    NOT NULL,
    mode          TEXT    NOT NULL DEFAULT 'all',
    collection_id INTEGER REFERENCES collections(id) ON DELETE SET NULL,
    uid_validity  INTEGER,
    last_uid      INTEGER NOT NULL DEFAULT 0,
    -- bis zu welcher emails.seq bekannte Konversationen hier schon gesucht wurden
    last_seq      INTEGER NOT NULL DEFAULT 0,
    UNIQUE (account_id, name)
);
