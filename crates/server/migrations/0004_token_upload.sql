-- Zugangsschlüssel dürfen optional Dokumente hochladen und taggen (nie löschen oder ändern)
ALTER TABLE api_tokens ADD COLUMN can_upload INTEGER NOT NULL DEFAULT 0;
