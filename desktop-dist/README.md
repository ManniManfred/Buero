Installationsdateien des Desktop-Clients (`.exe`, `.msi`, `.deb`, `.rpm`, `.AppImage`),
die der Server unter `/desktop/` zum Herunterladen anbietet (Einstellungen →
„Desktop-App“). Liegt daneben eine Signatur (`<datei>.sig`), bietet der Server
die Datei auch als automatisches Update an (`/api/desktop/update`). Der
Release-Workflow legt beides hier ab, bevor das Docker-Image gebaut wird.
Ordner auf dem Server: `BUERO_DESKTOP_DIR`.
