#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Startet Büro als Benutzer „buero“ statt als root. Fremde Dateien (PDFs,
# Bilder, Office-Dateien) werden so nie mit root-Rechten verarbeitet.
set -e

if [ "$(id -u)" = "0" ]; then
  data="${BUERO_DATA_DIR:-/data}"
  mkdir -p "$data"
  # Ältere Versionen liefen als root: Datenordner einmalig übergeben
  if [ -n "$(find "$data" ! -user buero -print -quit)" ]; then
    echo "Übergebe $data an den Benutzer buero …"
    chown -R buero:buero "$data"
  fi
  # Rechte abgeben; nur das Öffnen von Port 80 bleibt erlaubt
  exec setpriv --reuid=buero --regid=buero --init-groups \
    --inh-caps=-all,+net_bind_service --ambient-caps=-all,+net_bind_service \
    --bounding-set=-all,+net_bind_service -- "$@"
fi
exec "$@"
