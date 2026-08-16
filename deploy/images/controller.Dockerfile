ARG AURSMITH_SOURCE_GIT_COMMIT=unknown
FROM node:22.22.2-bookworm-slim@sha256:9f6d5975c7dca860947d3915877f85607946403fc55349f39b4bc3688448bb6e AS web-builder
WORKDIR /src
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web ./
RUN npm run build

FROM rust:1.97.1-bookworm@sha256:14bc9c5966e7b3a385794b3d5389a8765668342025fbcc7b2e3d2866ac4bd8c3 AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY migrations ./migrations
COPY crates ./crates
RUN cargo build --locked --release -p aursmith-controller -p aursmithctl

FROM debian:bookworm-slim@sha256:abd67ffcfa541b485a3dff59865ab629aa048a6c613e639d36e7456b0b229241
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates openssh-client openssh-server openssl rsync util-linux \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home --home-dir /var/lib/aursmith --shell /bin/sh aursmith \
    && install -d -o aursmith -g aursmith /run/aursmith
COPY --from=builder /src/target/release/aursmith-controller /usr/local/bin/aursmith-controller
COPY --from=builder /src/target/release/aursmithctl /usr/local/bin/aursmithctl
COPY --from=web-builder /src/dist /srv
USER 10001:10001
WORKDIR /var/lib/aursmith
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/aursmith-controller"]
CMD ["serve"]
ARG AURSMITH_SOURCE_GIT_COMMIT
LABEL org.opencontainers.image.revision=$AURSMITH_SOURCE_GIT_COMMIT
