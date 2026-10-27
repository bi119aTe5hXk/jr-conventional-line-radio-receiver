FROM rust:1-bookworm AS builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends libsoapysdr-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /source
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY src-tauri ./src-tauri
COPY ui ./ui
RUN cargo build --release --locked -p jr-receiver-web

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        libsoapysdr0.8 \
        soapysdr-module-rtlsdr \
        soapysdr-tools \
        usbutils \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /source/target/release/jr-receiver-web /usr/local/bin/jr-receiver-web
COPY ui /app/ui

ENV JR_RECEIVER_LISTEN=0.0.0.0:8080 \
    JR_RECEIVER_UI_DIR=/app/ui \
    JR_RECEIVER_CONFIG_DIR=/config \
    JR_RECEIVER_RECORDINGS_DIR=/recordings

VOLUME ["/config", "/recordings"]
EXPOSE 8080
CMD ["jr-receiver-web"]
