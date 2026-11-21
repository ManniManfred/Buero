# 📁 Büro – Dokumentenverwaltung

Schnelle, selbst gehostete Verwaltung für alle Dokumente – gescannte Briefe
genauso wie Word-, Excel- oder Markdown-Dateien. Komplett in **Rust**:

| Teil | Technik | Ordner |
|---|---|---|
| Server (HTTP-API) | [axum](https://github.com/tokio-rs/axum), SQLite ([sqlx](https://github.com/launchbadge/sqlx)), Volltextsuche mit [Tantivy](https://github.com/quickwit-oss/tantivy) | `crates/server` |
| Weboberfläche | [Leptos](https://leptos.dev) → WebAssembly | `crates/web` |
| Desktop-Client | [Tauri 2](https://tauri.app) mit **derselben** Oberfläche | `crates/desktop` |
| Gemeinsame Datentypen | serde | `crates/shared` |

Webseite und Desktop-Client sind derselbe Code: Der Desktop-Client lädt die
gleiche WebAssembly-Oberfläche und spricht dieselbe API an. Unterschiede gibt es
nur dort, wo es nötig ist (frei wählbare Server-Adresse, Dateien öffnen mit dem
Standardprogramm des Systems).

## Funktionen

- **Upload** per Drag & Drop (ins ganze Fenster) oder Dateiauswahl, mehrere
  Dateien parallel. Ist links eine Sammlung gewählt, landen Uploads direkt darin.
  Doppelte Dateien werden am SHA-256-Hash erkannt und nicht doppelt gespeichert.
- **Texterkennung** im Hintergrund:
  - PDF mit Textebene (`pdftotext`), gescannte PDFs und Bilder per **OCR**
    (Tesseract, Deutsch + Englisch)
  - Word (`.docx`), PowerPoint (`.pptx`), OpenDocument (`.odt`, `.odp`)
  - Excel / Tabellen (`.xlsx`, `.xls`, `.ods`)
  - Markdown, Text, CSV, HTML, RTF, JSON, …
- **Suche** (Tantivy, typischerweise wenige Millisekunden):
  - Suche während der Eingabe (letztes Wort als Wortanfang)
  - Deutsche Wortstämme (`Rechnungen` findet `Rechnung`), Umlaute egal (`kundigung` = `Kündigung`)
  - Automatisch tippfehlertolerant, wenn es keine exakten Treffer gibt
  - Phrasen `"kaution 1500"`, Ausschluss `-kfz`, `OR`, Feldsuche `title:`, `tags:`, `filename:`, `content:`
  - Treffer-Ausschnitte mit Hervorhebung
  - Durchsucht Titel, Dateiname, Notiz, Tags, Sammlungsnamen und Inhalt (unterschiedlich gewichtet)
  - Kombinierbar mit Filtern (Sammlung, Tags, Dateiart, „Eingang“) und Sortierung
- **Mehrfache Gruppierung**:
  - **Sammlungen** sind hierarchisch (beliebig tief) – und ein Dokument kann in
    **beliebig vielen** Sammlungen gleichzeitig liegen (z. B. *Finanzen / Rechnungen*
    **und** *Steuererklärung 2026* **und** *Wohnung*). Filtern nach einer Sammlung
    zeigt auch alles aus ihren Untersammlungen.
  - **Tags** (farbig) zusätzlich, ebenfalls beliebig viele pro Dokument.
  - **Eingang**: alle Dokumente, die noch keiner Sammlung zugeordnet sind.
  - Zuordnen per Drag & Drop (Dokument auf Sammlung ziehen), im Detailbereich
    oder für viele Dokumente gleichzeitig (Mehrfachauswahl).
- **Versionen**: bearbeitete Dateien als neue Version hochladen; alte Versionen
  bleiben abrufbar.
- Vorschau (PDF, Bilder, Text), Vorschaubilder in der Liste, Dokumentdatum,
  Notizen, Hell/Dunkel-Modus, Tastenkürzel (`/` = Suche, `Esc` = schließen).

## Installation auf CapRover

1. In CapRover eine neue App anlegen, z. B. `buero`, **„Has Persistent Data“ aktivieren**.
2. Unter *App Configs*:
   - **Persistent Directories**: Pfad in der App `/data` → Label z. B. `buero-data`
   - **Environment Variables**: `BUERO_PASSWORD=<dein-langes-passwort>`
   - Container HTTP Port: `80` (Standard)
3. Unter *HTTP Settings* HTTPS aktivieren und erzwingen. Für große Uploads ggf.
   in der NGINX-Konfiguration `client_max_body_size 1024m;` setzen.
4. Deployen – siehe nächster Abschnitt.

### Kontinuierliches Deployment (empfohlen)

Gebaut und ausgerollt wird **nur bei einem Versions-Tag** (`v*`): GitHub
Actions baut dann das Image (`ghcr.io/mannimanfred/buero:v1.2.3`, zusätzlich
`:1.2` und `:latest`) und rollt es automatisch auf CapRover aus
(`.github/workflows/docker.yml`). Normale Pushes auf `master` lösen nichts aus.
Der CapRover-Server muss nichts kompilieren.

Neue Version veröffentlichen:

```bash
git tag v0.2.0
git push origin v0.2.0
```

(oder auf GitHub unter *Releases → Draft a new release* einen neuen Tag
anlegen). Derselbe Tag baut auch die Desktop-Installer als Release-Entwurf.

Einmalige Einrichtung:

1. **Image öffentlich machen**: GitHub → Profil → *Packages* → `buero` →
   *Package settings* → *Change visibility* → *Public*. (Das Paket erscheint
   nach dem ersten Workflow-Lauf.) Alternativ privat lassen und in CapRover
   unter *Cluster* → *Docker Registries* `ghcr.io` mit GitHub-Benutzername und
   einem Personal Access Token (Recht `read:packages`) eintragen.
2. **App-Token in CapRover**: App `buero` → *Deployment* → *Method 1: Official
   CLI* → *Enable App Token* → Token kopieren.
3. **Secrets in GitHub**: Repository → *Settings* → *Secrets and variables* →
   *Actions* → *New repository secret*:

   | Secret | Beispiel |
   |---|---|
   | `CAPROVER_SERVER` | `https://captain.deine-domain.de` |
   | `CAPROVER_APP` | `buero` |
   | `CAPROVER_APP_TOKEN` | Token aus Schritt 2 |

4. Einen Tag pushen (siehe oben). Einen bestehenden Tag erneut ausrollen:
   *Actions* → *Docker-Image & CapRover-Deploy* → *Run workflow* und dort
   unter *Use workflow from* den Tag auswählen.

Jedes Deployment verwendet das Image mit der Versionsnummer, in CapRover ist
so unter *Deployment* jede Version nachvollziehbar und per *Revert*
zurückrollbar. Fehlen die Secrets, wird nur das Image gebaut.

Alternativ ohne GitHub Actions: `caprover deploy` im Projektordner baut über
`captain-definition` + `Dockerfile` direkt auf dem Server (braucht einige
Minuten und ca. 2–4 GB RAM).

### Konfiguration (Umgebungsvariablen)

| Variable | Standard | Bedeutung |
|---|---|---|
| `BUERO_PASSWORD` | – | Passwort für die Anmeldung. **Ohne** ist die App offen erreichbar! |
| `BUERO_DATA_DIR` | `/data` (Docker) | Datenbank, Dateien, Suchindex, Vorschaubilder |
| `PORT` / `BUERO_BIND` | `80` / `0.0.0.0:$PORT` | Adresse des HTTP-Servers |
| `BUERO_OCR` | `true` | Texterkennung für Scans/Bilder an/aus |
| `BUERO_OCR_LANG` | `deu+eng` | Tesseract-Sprachen |
| `BUERO_MAX_UPLOAD_MB` | `1024` | Maximale Dateigröße |
| `BUERO_WORKERS` | CPU-Kerne (max. 8) | Parallele Verarbeitung (Text/OCR) |
| `BUERO_SESSION_DAYS` | `90` | Gültigkeit einer Anmeldung |

### Datensicherung

Alles liegt in `/data`:
`buero.db` (SQLite: Metadaten, erkannter Text), `files/` (Originaldateien, nach
SHA-256 abgelegt), `thumbs/` und `index/` (beide jederzeit neu erzeugbar). Für
ein Backup genügt es, `buero.db*` und `files/` zu sichern. Fehlt der Suchindex,
wird er beim Start automatisch neu aufgebaut.

## Desktop-Client

Installationsdateien für Windows (`.msi`/`.exe`), macOS (`.dmg`) und Linux
(`.deb`, `.AppImage`) baut GitHub Actions beim Pushen eines Tags `v*`
(`.github/workflows/desktop.yml`) als Release-Entwurf.

Beim ersten Start fragt der Client nach der Server-Adresse
(z. B. `https://buero.deine-domain.de`), danach nach dem Passwort.

Lokal bauen:

```bash
cargo install tauri-cli trunk --locked
rustup target add wasm32-unknown-unknown
cd crates/desktop
cargo tauri build          # Installer unter target/release/bundle/
```

Unter Linux werden dafür `libwebkit2gtk-4.1-dev`, `librsvg2-dev` und
`libayatana-appindicator3-dev` benötigt.

## Entwicklung

```bash
rustup target add wasm32-unknown-unknown
cargo install trunk --locked

# Server (http://localhost:8080) – Text-/OCR-Werkzeuge optional:
#   apt install poppler-utils tesseract-ocr tesseract-ocr-deu
cargo run -p buero-server

# Oberfläche mit Live-Reload (http://localhost:8081, API wird an :8080 weitergeleitet)
cd crates/web && trunk serve

# Desktop-Client im Entwicklungsmodus (startet trunk serve selbst)
cd crates/desktop && cargo tauri dev

# Tests
cargo test
```

## HTTP-API

Alle Endpunkte (außer Anmeldung/Status) erwarten `Authorization: Bearer <token>`
(oder `?token=` für Datei-Links).

| Methode | Pfad | Zweck |
|---|---|---|
| `POST` | `/api/auth/login` | `{"password": "…"}` → `{"token": "…"}` |
| `GET` | `/api/auth/status` | Anmeldung nötig / gültig? |
| `GET` | `/api/documents?q=&collections=&tags=&kind=&unfiled=&sort=&offset=&limit=` | Suche & Liste |
| `POST` | `/api/documents` | Upload (multipart: `file` beliebig oft, optional `collection_ids`, `tag_ids`, `title`, `document_date`) |
| `GET/PATCH/DELETE` | `/api/documents/{id}` | Details / ändern (Titel, Notiz, Datum, Sammlungen, Tags) / löschen |
| `GET` | `/api/documents/{id}/file[?download=1&version=N]` | Datei (unterstützt Range-Requests) |
| `GET` | `/api/documents/{id}/thumbnail` · `/text` | Vorschaubild · erkannter Text |
| `POST` | `/api/documents/{id}/versions` | Neue Version hochladen |
| `POST` | `/api/documents/{id}/reprocess` | Texterkennung wiederholen |
| `POST` | `/api/documents/bulk` | Mehrere Dokumente: Sammlungen/Tags hinzufügen/entfernen, löschen |
| `GET/POST` | `/api/collections` · `PATCH/DELETE /api/collections/{id}` | Sammlungen (mit `parent_id`) |
| `GET/POST` | `/api/tags` · `PATCH/DELETE /api/tags/{id}` | Tags |
| `GET` | `/api/stats` | Anzahl, Größe, offene Verarbeitung |
| `POST` | `/api/admin/reindex` · `/api/admin/reprocess-failed` | Wartung |

Beispiel – Scan direkt aus einem Skript hochladen:

```bash
TOKEN=$(curl -s -X POST https://buero.example.com/api/auth/login \
  -H 'content-type: application/json' -d '{"password":"…"}' | jq -r .token)
curl -H "Authorization: Bearer $TOKEN" -F file=@scan.pdf https://buero.example.com/api/documents
```

## Lizenz

Copyright (C) 2026 Manfred Roßkamp

Dieses Programm ist freie Software: Sie können es unter den Bedingungen der
**GNU Affero General Public License**, wie von der Free Software Foundation
veröffentlicht, weitergeben und/oder modifizieren, entweder gemäß Version 3
der Lizenz oder (nach Ihrer Wahl) jeder späteren Version.

Dieses Programm wird in der Hoffnung verbreitet, dass es nützlich sein wird,
jedoch OHNE JEDE GEWÄHRLEISTUNG – sogar ohne die implizite Gewährleistung der
MARKTFÄHIGKEIT oder EIGNUNG FÜR EINEN BESTIMMTEN ZWECK. Details stehen in der
Datei [`LICENSE`](LICENSE).

Wer eine veränderte Version als Netzwerkdienst anbietet, muss den Nutzern den
Quellcode dieser Version zugänglich machen (§ 13 AGPL). Die Oberfläche verlinkt
dazu unter *Einstellungen* auf den Quellcode.

Die Lizenzen aller verwendeten Bibliotheken stehen in
[`crates/web/licenses.html`](crates/web/licenses.html) und sind in der
Anwendung unter *Einstellungen → Lizenzen von Drittanbietern* abrufbar
(Webseite, Desktop-Client und Docker-Image). Die Datei wird beim Docker- und
Desktop-Build automatisch neu erzeugt; nach Änderungen an Abhängigkeiten lokal
aktualisieren mit:

```bash
cargo install cargo-about --locked --features cli
cargo about generate --offline about.hbs -o crates/web/licenses.html
```

Das Docker-Image enthält zusätzlich `poppler-utils` (GPL) und Tesseract
(Apache-2.0) aus den Debian-Paketen; sie werden als eigenständige Programme
aufgerufen. Die Lizenzen der verwendeten Rust-Bibliotheken (überwiegend
MIT/Apache-2.0) sind mit der AGPL vereinbar.
