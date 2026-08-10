ARG AURSMITH_SOURCE_GIT_COMMIT=unknown
FROM rust:1.97.1-bookworm@sha256:14bc9c5966e7b3a385794b3d5389a8765668342025fbcc7b2e3d2866ac4bd8c3 AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY migrations ./migrations
COPY crates ./crates
RUN cargo build --locked --release -p aursmith-agent-gateway

FROM debian:bookworm-slim@sha256:abd67ffcfa541b485a3dff59865ab629aa048a6c613e639d36e7456b0b229241
ARG AURSMITH_SOURCE_GIT_COMMIT
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10003 --create-home --home-dir /var/lib/aursmith-agent-gateway gateway
COPY --from=builder /src/target/release/aursmith-agent-gateway /usr/local/bin/aursmith-agent-gateway
USER 10003:10003
ENTRYPOINT ["/usr/local/bin/aursmith-agent-gateway"]
LABEL org.opencontainers.image.revision=$AURSMITH_SOURCE_GIT_COMMIT
