-- Office-Vorschau ließ sich für diesen Dateiinhalt nicht erzeugen: beim
-- Nachholen nach dem Start nicht erneut versuchen (neue Version: neuer Hash)
ALTER TABLE documents ADD COLUMN preview_failed_sha TEXT;
