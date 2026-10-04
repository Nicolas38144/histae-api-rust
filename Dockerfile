# syntax=docker/dockerfile:1.7
FROM node:22.22.1-bookworm-slim AS codec
ENV PNPM_HOME=/pnpm
ENV PATH=$PNPM_HOME:$PATH
RUN corepack enable
WORKDIR /build/tools/photo-codec
COPY tools/photo-codec/package.json tools/photo-codec/pnpm-lock.yaml ./
RUN --mount=type=cache,id=pnpm,target=/pnpm/store pnpm install --prod --frozen-lockfile

FROM rust:1.88-bookworm AS builder
RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential perl nasm pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY db ./db
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --locked --release --features webauthn-probe --bins \
    && mkdir -p /artifacts \
    && cp target/release/api target/release/outbox target/release/maintenance \
          target/release/admin-bootstrap target/release/db-migrate target/release/storage-init \
          target/release/contract-compare /artifacts/

FROM node:22.22.1-bookworm-slim AS runtime
ENV HISTAE_RUNTIME_ROOT=/app
ENV PATH=/app/bin:$PATH
WORKDIR /app
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupmod --gid 1000 node \
    && usermod --uid 1000 --gid 1000 node
COPY --from=builder --chown=1000:1000 /artifacts/ /app/bin/
COPY --from=codec --chown=1000:1000 /build/tools/photo-codec/node_modules /app/tools/photo-codec/node_modules
COPY --chown=1000:1000 tools/photo-codec/package.json tools/photo-codec/processor.cjs /app/tools/photo-codec/
COPY --chown=1000:1000 tools/photo-codec-runner.cjs /app/tools/photo-codec-runner.cjs
USER 1000:1000
EXPOSE 8080 9091
ENTRYPOINT ["/app/bin/api"]
