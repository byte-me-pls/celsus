# The Celsus server: a shared, translating library for a home server or a Raspberry Pi.
# Builds with plain `docker build` (no BuildKit needed) and for several platforms with buildx:
#   docker build -t ghcr.io/byte-me-pls/celsus .
#   docker buildx build --platform linux/amd64,linux/arm64 -t ghcr.io/byte-me-pls/celsus .

ARG RUST_VERSION=1.94
ARG DEBIAN=trixie
# Fewer parallel compile jobs use less memory and CPU (a 4 GB Raspberry Pi, a busy server):
#   --build-arg BUILD_JOBS=2
ARG BUILD_JOBS=

# ---------- build ----------
FROM rust:${RUST_VERSION}-${DEBIAN} AS build
ARG BUILD_JOBS
WORKDIR /src
COPY . .
# Only the server is built; the desktop crate is in the workspace but never compiled here.
RUN cargo build --release --locked -p celsus-server ${BUILD_JOBS:+--jobs $BUILD_JOBS} \
    && install -D -m 0755 target/release/celsus-server /out/celsus-server

# ---------- run ----------
FROM debian:${DEBIAN}-slim
ARG VERSION=dev
LABEL org.opencontainers.image.title="Celsus" \
      org.opencontainers.image.description="A library that translates books without breaking their layout" \
      org.opencontainers.image.source="https://github.com/byte-me-pls/celsus" \
      org.opencontainers.image.version="${VERSION}"

# Fonts to write translations with: Noto for most writing systems, WenQuanYi Zen Hei (TrueType) for Chinese,
# Japanese and Korean; certificates for model providers over HTTPS.
RUN apt-get update \
    && apt-get install -y --no-install-recommends fonts-noto-core fonts-wqy-zenhei ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --user-group --home-dir /data --no-create-home --shell /usr/sbin/nologin celsus \
    && install -d -o celsus -g celsus -m 0700 /data

COPY --from=build /out/celsus-server /usr/local/bin/celsus-server
COPY crates/celsus-server/web /app/web
# Shared with the desktop app: styles, fonts, the reader and pdf.js.
COPY crates/celsus-desktop/ui/style.css crates/celsus-desktop/ui/reader.js crates/celsus-desktop/ui/logo.png crates/celsus-desktop/ui/mark.png /app/ui/
COPY crates/celsus-desktop/ui/fonts /app/ui/fonts
COPY crates/celsus-desktop/ui/i18n /app/ui/i18n
COPY crates/celsus-desktop/ui/pdfjs /app/ui/pdfjs

ENV CELSUS_ADDR=0.0.0.0:8080 \
    CELSUS_DATA_DIR=/data \
    CELSUS_WEB_DIR=/app/web \
    CELSUS_UI_DIR=/app/ui \
    CELSUS_FONTS_DIR=/usr/share/fonts \
    RUST_LOG=info

USER celsus:celsus
VOLUME ["/data"]
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 CMD ["celsus-server", "healthcheck"]
ENTRYPOINT ["celsus-server"]
