# syntax=docker/dockerfile:1
FROM rust:1.99-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
RUN cargo build --release --locked -p hermes-node

FROM debian:bookworm-slim
# A container node executes commands inside its own image. Mount the work roots
# you intend it to use; it does not get the host filesystem implicitly.
RUN apt-get update \
    && apt-get install -y --no-install-recommends bash ca-certificates coreutils procps \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 hermes \
    && useradd --uid 10001 --gid 10001 --create-home --shell /bin/bash hermes
COPY --from=build /src/target/release/hermes-node /usr/local/bin/hermes-node
USER 10001:10001
WORKDIR /home/hermes
ENV HERMES_NODE_ALLOWED_ROOTS=/home/hermes \
    RUST_LOG=hermes_node=info,warn
ENTRYPOINT ["/usr/local/bin/hermes-node"]
