# syntax=docker/dockerfile:1
FROM rust:1.99-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
RUN cargo build --release --locked -p hermes-hub

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 hermes \
    && useradd --uid 10001 --gid 10001 --no-create-home --shell /usr/sbin/nologin hermes \
    && install -d -o 10001 -g 10001 /data
COPY --from=build /src/target/release/hermes-hub /usr/local/bin/hermes-hub
USER 10001:10001
WORKDIR /data
ENV HERMES_HUB_BIND=0.0.0.0:8080 \
    HERMES_HUB_DB=/data/hermes.db
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/hermes-hub"]
