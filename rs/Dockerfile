# ticket-tracker headless server
# build:  docker build -t ticket-tracker .
# run:    docker run -p 127.0.0.1:8787:8787 -v ticket-data:/data ticket-tracker
# note:   Gofer must run natively on macOS (WeChat/XHS GUI), not in Docker.

FROM rust:1-bookworm AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential pkg-config libsqlite3-dev && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
COPY templates ./templates
RUN cargo build --release --locked --bin tt-server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates tzdata curl && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/tt-server /usr/local/bin/tt-server
COPY web /app/web
ENV TT_BIND=0.0.0.0:8787 \
    TT_DB=/data/ticket-tracker.db \
    TT_WEB_DIR=/app/web \
    TZ=Asia/Shanghai
VOLUME ["/data"]
EXPOSE 8787
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8787/api/v1/health || exit 1
ENTRYPOINT ["tt-server"]
