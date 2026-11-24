-- Änderungsdatum der hochgeladenen Datei (Unix-Sekunden), z. B. aus dem
-- Sync-Ordner. Damit bekommt die Datei beim Herunterladen wieder ihr Datum
-- statt des Hochladezeitpunkts. NULL: unbekannt (dann gilt updated_at).
ALTER TABLE documents ADD COLUMN file_mtime INTEGER;
