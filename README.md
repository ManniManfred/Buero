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
Standardprogramm des Systems, Sammlungen als Laufwerk – `crates/vfs`).

## Funktionen

- **Upload** per Drag & Drop (ins ganze Fenster) oder Dateiauswahl, mehrere
  Dateien parallel. Ist links eine Sammlung gewählt, landen Uploads direkt darin.
  Doppelte Dateien werden am SHA-256-Hash erkannt und nicht doppelt gespeichert.
- **Scannen mit der Handy-Kamera** direkt im Browser (Knopf „Scannen“, Kamera-Symbol):
  mehrere Seiten nacheinander aufnehmen, Blattränder werden automatisch erkannt
  und lassen sich mit Lupe von Hand nachziehen; die Seiten werden perspektivisch
  entzerrt, aufgehellt (Filter *Dokument*, *Farbe*, *Graustufen*, *Original*),
  lassen sich drehen, umsortieren und löschen und werden als **eine PDF** hochgeladen
  (danach OCR wie bei jedem Scan). Läuft komplett im Browser, braucht HTTPS; ohne
  Kamerazugriff geht es über die Kamera-App oder Bilder aus der Galerie.
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
- **Freigaben**: Links zu einem Dokument oder einer ganzen Sammlung (inkl.
  Untersammlungen) für Leute ohne Zugang – wahlweise mit Passwort, Ablaufdatum
  und (bei Sammlungen) Upload-Möglichkeit, z. B. für den Steuerberater. Links
  lassen sich jederzeit widerrufen; Büro zählt die Aufrufe.
- **KI-Zugang (MCP)**: KI-Assistenten wie Claude können unter `/mcp` (Model
  Context Protocol, Streamable HTTP) suchen und erkannten Text lesen – nur
  lesend und nur mit einem Zugangsschlüssel aus den Einstellungen (gilt
  nirgends sonst, jederzeit widerrufbar). Sammlungen lassen sich für den
  KI-Zugang **sperren**: Ihre Dokumente samt Untersammlungen werden nie
  ausgeliefert, auch nicht als Treffer oder in Zählungen – selbst wenn sie
  zusätzlich in anderen Sammlungen liegen. Einrichten in Claude Code:
  `claude mcp add --transport http buero https://…/mcp --header "Authorization: Bearer bmcp_…"`
- **Datensicherung**: Backup als tar-Archiv herunterladen und wieder einspielen,
  direkt in der Oberfläche.
- **Bearbeiten mit Euro-Office** (OnlyOffice-kompatibel): Word-, Excel- und
  PowerPoint-Dateien sowie OpenDocument direkt im Browser bearbeiten. Beim
  Schließen (oder mit „Speichern“) legt Büro eine neue Version an.
- **Bearbeiten mit dem Tupfer Designer**: tiptoi-Projekte (`.tupfer`) direkt im
  Browser bearbeiten ([Tupfer](https://github.com/ManniManfred/Tupfer), liegt dem
  Docker-Image bei). „Speichern“ im Designer legt eine neue Version an; hat
  inzwischen jemand anderes gespeichert, fragt der Designer nach.
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
Actions baut dann zuerst den Desktop-Client, danach das Image
(`ghcr.io/mannimanfred/buero:v1.2.3`, zusätzlich `:1.2` und `:latest`) mit
dessen Installationsdateien und rollt es automatisch auf CapRover aus
(`.github/workflows/release.yml`). Normale Pushes auf `master` lösen nichts aus.
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
| `BUERO_SHARE_MAX_UPLOAD_MB` | `100` | Maximale Dateigröße beim Hochladen über einen Freigabelink |
| `BUERO_SHARE_UPLOADS_PER_HOUR` | `50` | Höchstens so viele Dateien pro Stunde und Freigabelink |
| `BUERO_OCR_MAX_PAGES` | `100` | Bei gescannten PDFs höchstens so viele Seiten per OCR erkennen |
| `BUERO_WORKERS` | CPU-Kerne (max. 8) | Parallele Verarbeitung (Text/OCR) |
| `BUERO_SESSION_DAYS` | `90` | Gültigkeit einer Anmeldung |
| `BUERO_PUBLIC_URL` | aus Anfrage | Öffentliche Adresse, z. B. `https://buero.example.com` – für Euro-Office empfohlen |
| `BUERO_OFFICE_URL` | – | Adresse des Euro-Office/OnlyOffice Document Servers, z. B. `https://eurooffice.example.com` |
| `BUERO_OFFICE_JWT_SECRET` | – | JWT-Schlüssel des Document Servers (dort `JWT_SECRET`) |
| `BUERO_DESKTOP_DIR` | `/app/desktop` (Docker) | Installationsdateien des Desktop-Clients, werden unter `/desktop/` zum Herunterladen angeboten |
| `BUERO_TUPFER_DIR` | `/app/tupfer` (Docker) | Web-Version des Tupfer Designers, wird unter `/tupfer/` ausgeliefert. Fehlt sie, gibt es kein „Bearbeiten“ für `.tupfer` |

### Sicherheit

- **Passwort**: `BUERO_PASSWORD` schützt alles – ein langes Zufallspasswort
  (20+ Zeichen) verwenden. Nach 5 Fehlversuchen wird die IP-Adresse gesperrt
  (30 s, dann immer länger bis 1 h); parallele Versuche einer Adresse werden
  abgelehnt. Hinter CapRover/nginx zählt die echte Adresse aus `X-Forwarded-For`.
  Dasselbe gilt für Passwörter von Freigabelinks (gespeichert als Argon2-Hash).
- **Abmelden**: Wird `BUERO_PASSWORD` geändert, sind beim nächsten Start alle
  Anmeldungen beendet. In den Einstellungen meldet „Überall abmelden“ alle
  Geräte ab.
- **Keine Zugangsdaten in URLs**: Vorschau, Vorschaubilder und Downloads nutzen
  einen Datei-Schlüssel, der nur Dateien lesen kann und nach 6–12 Stunden
  abläuft; der Backup-Download einen 5 Minuten gültigen Link.
- **Freigabelinks mit Upload**: Dateigröße und Anzahl pro Stunde sind begrenzt
  (siehe oben). Ob eine Datei schon vorhanden war, erfährt der Hochladende nicht.
- **Dateiverarbeitung**: Zeitlimits für pdftotext/pdftoppm/tesseract, OCR nur
  für die ersten `BUERO_OCR_MAX_PAGES` Seiten, Schutz vor „Zip-Bomben“ in
  Office-Dateien.
- **Browser**: Content-Security-Policy, Einbetten nur durch die eigene Seite,
  API-Aufrufe aus fremden Webseiten (CORS) nur für den Desktop-Client.
- **Docker**: Der Server läuft als Benutzer `buero`, nicht als root. Beim
  ersten Start nach dem Update übernimmt er den Datenordner (`chown`).
- **Backups** enthalten alle Dokumente und `secret.key` unverschlüsselt – so
  sicher aufbewahren wie den Server selbst.

### Euro-Office anbinden

1. In der CapRover-App von Euro-Office den Wert der Umgebungsvariable
   `JWT_SECRET` nachsehen (ist er nicht gesetzt, dort einen langen Zufallswert
   eintragen und neu starten).
2. In der Büro-App setzen:
   `BUERO_OFFICE_URL=https://eurooffice.example.com`,
   `BUERO_OFFICE_JWT_SECRET=<derselbe Wert>`,
   `BUERO_PUBLIC_URL=https://buero.example.com`.
3. Der Document Server muss Büro unter `BUERO_PUBLIC_URL` erreichen können
   (er lädt darüber die Datei und meldet Änderungen zurück).

Im Detailbereich erscheint dann „Bearbeiten“. Gespeichert wird beim Schließen
des Editors (nach wenigen Sekunden) oder sofort mit dem Speichern-Knopf in
Euro-Office – jeweils als neue Version.

### Tupfer Designer

Das Docker-Image holt beim Bauen die Web-Version des
[Tupfer Designers](https://github.com/ManniManfred/Tupfer) aus dessen neuestem
Release und liefert sie unter `/tupfer/` aus. Bei `.tupfer`-Dokumenten erscheint
dann im Detailbereich „Bearbeiten“. Der Designer lädt und speichert selbst über
die API von Büro (auch im Desktop-Client). Eine feste Version lässt sich beim
Bauen wählen:

```bash
docker build --build-arg TUPFER_URL=https://github.com/ManniManfred/Tupfer/releases/download/v1.37.0/Tupfer-Designer-Nextcloud.tar.gz .
```

Ohne Docker: die Web-Version (`dotnet publish Tupfer.Designer.Browser`, Ordner
`wwwroot`) nach `./tupfer` legen oder `BUERO_TUPFER_DIR` setzen.

### Datensicherung

Alles liegt in `/data`:
`buero.db` (SQLite: Metadaten, erkannter Text), `files/` (Originaldateien, nach
SHA-256 abgelegt), `secret.key` (Schlüssel für signierte Links), `thumbs/` und
`index/` (beide jederzeit neu erzeugbar).

**In der Oberfläche** (Einstellungen → Datensicherung):

- **Backup herunterladen** liefert ein tar-Archiv (`buero-backup-JJJJ-MM-TT.tar`)
  mit einer konsistenten Kopie der Datenbank, allen Dateien, Vorschaubildern und
  `secret.key`. Das Archiv wird direkt gestreamt und braucht auf dem Server keinen
  zusätzlichen Platz. Das Backup kann während des Betriebs erstellt werden.
- **Aus Backup wiederherstellen** lädt ein solches Archiv hoch und prüft es:
  Format, Datenbank-Integrität und ob die Büro-Version zur Datenbank passt.
  Danach startet der Server im selben Prozess neu, übernimmt den Stand und baut
  den Suchindex neu auf. Wer angemeldet ist, bleibt angemeldet. Der bisherige
  Stand bleibt in `/data/vor-wiederherstellung/` liegen. Dort kann man ihn
  löschen, sobald alles passt. Er wird bei der nächsten Wiederherstellung ersetzt.

Per Kommandozeile geht das Gleiche:

```bash
curl -fH "Authorization: Bearer $TOKEN" -o backup.tar https://buero.example.com/api/admin/backup
curl -fH "Authorization: Bearer $TOKEN" --data-binary @backup.tar https://buero.example.com/api/admin/restore
```

Alternativ genügt es, `buero.db*`, `files/` und `secret.key` bei gestopptem
Server zu kopieren. Fehlt der Suchindex, wird er beim Start automatisch neu
aufgebaut.

## Desktop-Client

Installationsdateien für Windows (`.msi`/`.exe`) und Linux (`.deb`, `.rpm`,
`.AppImage`) baut GitHub Actions beim Pushen eines Tags `v*`
(`.github/workflows/release.yml`) als Release-Entwurf.

**Download über die Weboberfläche:** Das Docker-Image enthält die
Installationsdateien derselben Version. Die Webseite bietet sie unter
*Einstellungen → Desktop-App* an (passend zum eigenen System hervorgehoben)
und auf der Anmeldeseite als Link. Ohne Docker die Dateien in den Ordner
`BUERO_DESKTOP_DIR` (Standard `./desktop-dist`) legen; ist er leer, verweist
die Webseite auf die GitHub-Releases.

Beim ersten Start fragt der Client nach der Server-Adresse
(z. B. `https://buero.deine-domain.de`), danach nach dem Passwort.

### Automatische Updates

Der Desktop-Client holt Updates von **seinem eigenen Büro-Server**
(`/api/desktop/update`) und passt damit immer zu dessen Version: Nach jeder
Anmeldung fragt er nach, ob der Server eine neuere Version mitbringt, und
bietet sie unten rechts an (*Jetzt aktualisieren* – herunterladen,
installieren, neu starten). Von Hand: *Einstellungen → Updates*.

Installiert wird nur, was mit dem Update-Schlüssel signiert ist. Einmalig
einrichten:

1. Schlüsselpaar erzeugen (Passwort merken):
   `cargo tauri signer generate -w ~/.tauri/buero-updater.key`
2. Inhalt von `~/.tauri/buero-updater.key` als GitHub-Secret
   `TAURI_SIGNING_PRIVATE_KEY` hinterlegen, das Passwort als
   `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.
3. Den öffentlichen Schlüssel (`~/.tauri/buero-updater.key.pub`) in
   `crates/desktop/tauri.conf.json` unter `plugins.updater.pubkey` eintragen.

Der Release-Workflow signiert dann alle Installationsdateien (`*.sig`) und
legt sie mit ins Docker-Image. Ohne Secret wird weiter gebaut, nur ohne
Updates; ohne `pubkey` sucht der Client nicht nach Updates. Der private
Schlüssel darf nicht verloren gehen – mit einem neuen Schlüssel lassen sich
bestehende Installationen nicht mehr automatisch aktualisieren.

Aktualisiert wird im selben Format, in dem die App installiert wurde
(Windows-Installer bzw. MSI, AppImage, `.deb`, `.rpm`; bei `.deb`/`.rpm` fragt
Linux nach dem Administrator-Passwort). Die App-Version kommt beim Release
aus dem Git-Tag.

### Sammlungen als Laufwerk

Unter *Einstellungen → Laufwerk* bindet der Desktop-Client die Sammlungen als
Ordner ins System ein – unter Linux standardmäßig nach `~/Büro`, unter Windows
als Laufwerk `B:` (oder der nächste freie Buchstabe). Danach wird es bei jedem
Start automatisch eingebunden, bis man es wieder aushängt.

- Jede Sammlung ist ein Ordner, Untersammlungen sind Unterordner. Dokumente ohne
  Sammlung liegen im Ordner `Eingang`. Liegt ein Dokument in mehreren
  Sammlungen, erscheint es in jedem dieser Ordner.
- Dateinamen sind der Titel plus die Endung der Originaldatei. Dateien werden
  erst beim Öffnen geladen und zwischengespeichert.
- **Ändern** einer Datei legt beim Schließen eine neue Version an. Hat jemand
  anderes inzwischen gespeichert, landet die eigene Fassung als
  „… (Konflikt)“ daneben.
- **Hineinkopieren** lädt in die Sammlung des Ordners hoch (Wurzel und
  `Eingang`: ohne Sammlung).
- **Umbenennen** ändert den Titel, **Verschieben** wechselt die Sammlung.
  Ordner anlegen, umbenennen, verschieben und (leer) löschen wirkt auf die
  Sammlungen.
- **Löschen** nimmt ein Dokument nur aus der Sammlung. Nur im Ordner `Eingang`
  wird es endgültig gelöscht.
- Temporäre Dateien (Sperrdateien wie `~$…` oder `.~lock…`, `*.tmp`, versteckte
  Dateien) bleiben lokal und werden nicht hochgeladen. Damit klappt das
  Speichern aus Word, Excel, LibreOffice & Co. direkt im Laufwerk.
- Änderungen am Server (z. B. über die Weboberfläche) erscheinen nach wenigen
  Sekunden.

Voraussetzungen:

- **Linux:** FUSE 3 (Paket `fuse3`, bei den gängigen Distributionen
  vorinstalliert).
- **Windows:** [WinFsp](https://winfsp.dev/rel/) einmalig installieren. Fehlt
  es, zeigt Büro in den Einstellungen einen Link zum Herunterladen.

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

# Laufwerk ohne Desktop-Client testen (Linux; „-“ = ohne Passwort)
cargo run -p buero-vfs --example mount -- http://localhost:8080 - /tmp/buero-mnt

# Tests
cargo test
```

## HTTP-API

Alle Endpunkte (außer Anmeldung/Status und Desktop-Download) erwarten `Authorization: Bearer <token>`
(oder `?token=` für Datei-Links).

| Methode | Pfad | Zweck |
|---|---|---|
| `POST` | `/api/auth/login` | `{"password": "…"}` → `{"token": "…"}` |
| `GET` | `/api/auth/status` | Anmeldung nötig / gültig? |
| `GET` | `/api/dashboard` | Übersicht: Speicherplatz (Datenträger, Belegung nach Bereichen) und Kennzahlen |
| `GET` | `/api/desktop` · `/desktop/{datei}` | Angebotene Installationsdateien des Desktop-Clients · Download (ohne Anmeldung) |
| `GET` | `/api/desktop/update?target=&arch=&bundle=&version=` | Update für den Desktop-Client (Tauri-Updater-Format, `204` = keins) |
| `GET` | `/api/documents?q=&collections=&tags=&kind=&unfiled=&sort=&offset=&limit=` | Suche & Liste |
| `POST` | `/api/documents` | Upload (multipart: `file` beliebig oft, optional `collection_ids`, `tag_ids`, `title`, `document_date`) |
| `GET/PATCH/DELETE` | `/api/documents/{id}` | Details / ändern (Titel, Notiz, Datum, Sammlungen, Tags) / löschen |
| `GET` | `/api/documents/{id}/file[?download=true&version=N]` | Datei (unterstützt Range-Requests) |
| `GET` | `/api/documents/{id}/thumbnail` · `/text` | Vorschaubild · erkannter Text |
| `POST` | `/api/documents/{id}/versions` | Neue Version hochladen |
| `POST` | `/api/documents/{id}/reprocess` | Texterkennung wiederholen |
| `POST` | `/api/documents/bulk` | Mehrere Dokumente: Sammlungen/Tags hinzufügen/entfernen, löschen |
| `GET/POST` | `/api/collections` · `PATCH/DELETE /api/collections/{id}` | Sammlungen (mit `parent_id`) |
| `GET/POST` | `/api/tags` · `PATCH/DELETE /api/tags/{id}` | Tags |
| `GET` | `/api/stats` | Anzahl, Größe, offene Verarbeitung |
| `GET/POST` | `/api/shares[?document_id=&collection_id=]` · `DELETE /api/shares/{id}` | Freigabelinks |
| `GET` | `/api/features` · `/api/office/config/{id}` | Aktive Zusatzfunktionen · Euro-Office-Editor-Konfiguration |
| `POST` | `/api/admin/reindex` · `/api/admin/reprocess-failed` | Wartung |
| `GET` | `/api/admin/backup` | Backup als tar-Archiv |
| `POST` | `/api/admin/restore` | Backup einspielen (Archiv als Anfragekörper), danach Neustart |

Ohne Anmeldung erreichbar (über Token, Passwort-Schlüssel bzw. signierte
Links geschützt): `/api/public/shares/{token}…` für Freigabelinks (Seite
`/s/{token}`) sowie `/api/office/file/{id}` und `/api/office/callback/{id}` für
den Document Server.

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
