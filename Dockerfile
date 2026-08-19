FROM rust:1-slim-bookworm AS rust-builder
WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY cli/Cargo.toml ./cli/Cargo.toml
COPY src ./src
# Only the server is built here; the CLI ships as a separate release binary.
RUN cargo build --release --package api

FROM debian:bookworm-slim
WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --home-dir /app --uid 10001 drop

COPY --from=rust-builder /app/target/release/api /usr/local/bin/drop-server

ENV DROP_BIND_ADDR=0.0.0.0:8080
EXPOSE 8080

USER drop

CMD ["drop-server"]
