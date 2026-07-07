# Frontend build stage
FROM node:20-slim AS frontend
WORKDIR /app
RUN npm install -g pnpm@9
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile
COPY . .
RUN pnpm build

# Backend build stage
FROM rust:latest AS builder
WORKDIR /app

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

COPY src-tauri/Cargo.toml src-tauri/Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && echo "" > src/lib.rs
RUN cargo build --release --features server --no-default-features 2>/dev/null || true

COPY . .
RUN cd src-tauri && cargo build --release --features server --no-default-features

# Runtime stage - use a newer base to match glibc from builder
FROM debian:trixie-slim

RUN apt-get update && apt-get install -y \
    libssl3 \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/src-tauri/target/release/ai-proxy-server /usr/local/bin/ai-proxy-server
COPY --from=frontend /app/dist /app/static

RUN mkdir -p /data && chmod 755 /data

ENV AI_PROXY_DATA_DIR=/data
ENV AI_PROXY_STATIC_DIR=/app/static
ENV RUST_LOG=info

EXPOSE 7860

VOLUME ["/data"]

ENTRYPOINT ["ai-proxy-server", "--host", "0.0.0.0", "--port", "7860"]
