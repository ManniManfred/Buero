-- Ordner ohne feste Sammlung: eigene Unter-Sammlung in der Sammlung des Postfachs
ALTER TABLE mail_folders ADD COLUMN own_collection INTEGER NOT NULL DEFAULT 1;
-- die dafür angelegte Sammlung (bleibt auch nach Umbenennen zugeordnet)
ALTER TABLE mail_folders ADD COLUMN auto_collection_id INTEGER REFERENCES collections(id) ON DELETE SET NULL;
-- schon übernommene Mails beim nächsten Abruf in die Unter-Sammlung einsortieren
ALTER TABLE mail_folders ADD COLUMN resort INTEGER NOT NULL DEFAULT 0;
UPDATE mail_folders SET resort = 1 WHERE collection_id IS NULL AND last_uid > 0;
