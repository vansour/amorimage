# syntax=docker/dockerfile:1

FROM node:26-trixie AS frontend
WORKDIR /app/frontend
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run build

FROM rust:trixie AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY --from=frontend /app/frontend/dist ./frontend/dist
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release --locked \
    && cp target/release/amorimage /amorimage

FROM debian:trixie-slim
# 调用 WebDAV / S3 走 HTTPS 需要根证书；slim 镜像没带，缺了连 HTTP 客户端都建不出来
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /amorimage /usr/local/bin/amorimage
COPY docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN useradd --system --home-dir /nonexistent --shell /usr/sbin/nologin amorimage \
    && chmod +x /usr/local/bin/docker-entrypoint.sh
ENV AMOR_BIND=0.0.0.0:3000 \
    AMOR_DB_DIR=/var/lib/amorimage/db \
    AMOR_STORAGE_DIR=/var/lib/amorimage/storage
EXPOSE 3000
ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
