# syntax=docker/dockerfile:1
# ---------------------------------------------------------------------------
# Büro – Server + Weboberfläche in einem Image (für CapRover & Co.)
# ---------------------------------------------------------------------------
ARG RUST_IMAGE=rust:1-bookworm
ARG RUNTIME_IMAGE=debian:bookworm-slim

FROM ${RUST_IMAGE} AS builder

RUN rustup target add wasm32-unknown-unknown
# trunk (Web-Build-Tool): vorkompiliertes Binary, sonst aus Quellcode
ARG TRUNK_VERSION=0.22.2
RUN set -eux; arch="$(uname -m)"; \
    url="https://github.com/trunk-rs/trunk/releases/download/v${TRUNK_VERSION}/trunk-${arch}-unknown-linux-gnu.tar.gz"; \
    if curl -fsSL "$url" | tar -xz -C /usr/local/bin; then trunk --version; \
    else cargo install --locked trunk; fi

WORKDIR /src
# Abhängigkeiten zuerst (besseres Layer-Caching)
COPY Cargo.toml Cargo.lock ./
COPY crates/shared/Cargo.toml crates/shared/
COPY crates/server/Cargo.toml crates/server/
COPY crates/web/Cargo.toml crates/web/
RUN mkdir -p crates/shared/src crates/server/src crates/web/src \
 && echo "" > crates/shared/src/lib.rs \
 && echo "fn main(){}" > crates/server/src/main.rs \
 && echo "fn main(){}" > crates/web/src/main.rs \
 && sed -i 's#, "crates/desktop", "crates/vfs"##' Cargo.toml \
 && cargo build --release -p buero-server \
 && cargo build --release -p web --target wasm32-unknown-unknown \
 && rm -rf crates/*/src

COPY crates/shared crates/shared
COPY crates/server crates/server
COPY crates/web crates/web
# Version aus dem Git-Tag (leer: Version aus Cargo.toml)
ARG BUERO_VERSION=
ENV BUERO_VERSION=${BUERO_VERSION}
# crates/web/licenses.html ist eingecheckt (von Hand aktualisiert, siehe README)
RUN touch crates/shared/src/lib.rs crates/server/src/main.rs crates/web/src/main.rs \
 && cd crates/web && trunk build --release \
 && cd /src && cargo build --release -p buero-server

# ---------------------------------------------------------------------------
# Tupfer Designer (Web-Version) zum Bearbeiten von .tupfer-Projekten: aus dem
# Release-Paket für Nextcloud (enthält die Web-Version im Ordner editor/).
# Feste Version z. B. mit
#   --build-arg TUPFER_URL=https://github.com/ManniManfred/Tupfer/releases/download/v1.37.0/Tupfer-Designer-Nextcloud.tar.gz
FROM ${RUNTIME_IMAGE} AS tupfer
ARG TUPFER_URL=https://github.com/ManniManfred/Tupfer/releases/latest/download/Tupfer-Designer-Nextcloud.tar.gz
ADD ${TUPFER_URL} /tmp/tupfer.tar.gz
RUN tar -xzf /tmp/tupfer.tar.gz -C /tmp \
 && test -f /tmp/tupfer/editor/index.html \
 && mv /tmp/tupfer/editor /tupfer

# ---------------------------------------------------------------------------
FROM ${RUNTIME_IMAGE}

LABEL org.opencontainers.image.title="Büro" \
      org.opencontainers.image.description="Dokumentenverwaltung" \
      org.opencontainers.image.authors="Manfred Roßkamp" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later" \
      org.opencontainers.image.source="https://github.com/ManniManfred/Buero"

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      poppler-utils tesseract-ocr tesseract-ocr-deu tesseract-ocr-eng ca-certificates tini \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system buero \
 && useradd --system --gid buero --home-dir /data --no-create-home --shell /usr/sbin/nologin buero

COPY docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod 755 /usr/local/bin/docker-entrypoint.sh
COPY --from=builder /src/target/release/buero-server /usr/local/bin/buero-server
COPY --from=builder /src/crates/web/dist /app/web
COPY --from=tupfer /tupfer /app/tupfer
# Installationsdateien des Desktop-Clients (vom Release-Workflow abgelegt, lokal meist leer)
COPY desktop-dist/ /app/desktop/

ENV BUERO_DATA_DIR=/data \
    BUERO_WEB_DIR=/app/web \
    BUERO_TUPFER_DIR=/app/tupfer \
    BUERO_DESKTOP_DIR=/app/desktop \
    PORT=80 \
    RUST_LOG=info

VOLUME ["/data"]
EXPOSE 80
HEALTHCHECK --interval=30s --timeout=5s CMD ["/bin/bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/${PORT} && printf 'GET /api/health HTTP/1.0\\r\\n\\r\\n' >&3 && grep -q ok <&3"]

# Startet als root, übergibt /data an „buero“ und läuft dann ohne root-Rechte
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/docker-entrypoint.sh"]
CMD ["buero-server"]
