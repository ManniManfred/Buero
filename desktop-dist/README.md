Optional: eigene Installationsdateien des Desktop-Clients (`.exe`, `.msi`,
`.deb`, `.rpm`, `.AppImage`), die der Server unter `/desktop/` zum
Herunterladen anbietet (Einstellungen → „Desktop-App“). Liegt daneben eine
Signatur (`<datei>.sig`), bietet der Server die Datei auch als automatisches
Update an (`/api/desktop/update`). Ordner auf dem Server: `BUERO_DESKTOP_DIR`.

Normalerweise bleibt der Ordner leer: Dann verlinkt der Server die Dateien aus
dem GitHub-Release zu seiner Version (`BUERO_DESKTOP_REPO`).
