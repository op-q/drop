FROM node:22-alpine AS web-builder
WORKDIR /app/web

COPY web/package.json web/package-lock.json ./
RUN npm ci

COPY web ./
RUN npm run build

FROM rust:1-slim-bookworm AS rust-builder
WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --home-dir /app --uid 10001 drop

COPY --from=rust-builder /app/target/release/api /usr/local/bin/drop
COPY --from=web-builder /app/web/dist /app/web/dist

ENV DROP_BIND_ADDR=0.0.0.0:8080
EXPOSE 8080

USER drop

CMD ["drop"]
