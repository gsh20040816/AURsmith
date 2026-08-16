ARG AURSMITH_SOURCE_GIT_COMMIT=unknown
FROM rust:1.97.1-bookworm@sha256:14bc9c5966e7b3a385794b3d5389a8765668342025fbcc7b2e3d2866ac4bd8c3 AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY migrations ./migrations
COPY crates ./crates
RUN cargo build --locked --release -p aursmith-agent-runner

FROM node:22-bookworm-slim@sha256:d649c27dae7ba0137b3cef5dd75baa422c08dc3d9e3fc0c23dfb172dc3cc6436
ARG AURSMITH_SOURCE_GIT_COMMIT
ARG CODEX_CLI_VERSION=0.147.0
ARG CLAUDE_CODE_VERSION=2.1.226
RUN apt-get update \
    && apt-get install --yes --no-install-recommends diffutils \
    && rm -rf /var/lib/apt/lists/* \
    && npm install --global --omit=dev \
      @openai/codex@${CODEX_CLI_VERSION} \
      @anthropic-ai/claude-code@${CLAUDE_CODE_VERSION} \
    && npm cache clean --force \
    && useradd --system --uid 10002 --create-home --home-dir /var/lib/aursmith-agent agent
COPY --from=builder /src/target/release/aursmith-agent-runner /usr/local/bin/aursmith-agent-runner
USER 10002:10002
WORKDIR /var/lib/aursmith-agent
ENTRYPOINT ["/usr/local/bin/aursmith-agent-runner"]
LABEL org.opencontainers.image.revision=$AURSMITH_SOURCE_GIT_COMMIT
