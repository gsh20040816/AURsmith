ARG AURSMITH_SOURCE_GIT_COMMIT=unknown
FROM rust:1.97.1-bookworm@sha256:14bc9c5966e7b3a385794b3d5389a8765668342025fbcc7b2e3d2866ac4bd8c3 AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY migrations ./migrations
COPY crates ./crates
RUN cargo build --locked --release -p aursmith-worker -p aursmithctl

FROM archlinux:base@sha256:345a872f6c95e082d4b8c050af637eebb57402c6e2177b411c3acf7df84eb33b
RUN pacman -Syu --noconfirm --needed \
      binutils ca-certificates docker fakeroot git gnupg openssh python rsync \
    && rm -rf /var/cache/pacman/pkg/* /var/lib/pacman/sync/* \
    && useradd --uid 10001 --create-home --home-dir /var/lib/aursmith --shell /bin/sh aursmith \
    && install -d -o aursmith -g aursmith \
      /run/aursmith /var/lib/aursmith/runtime /jobs \
      /landing /staging /repository
COPY --from=builder /src/target/release/aursmith-worker /usr/local/bin/aursmith-worker
COPY --from=builder /src/target/release/aursmithctl /usr/local/bin/aursmithctl
USER 10001:10001
WORKDIR /var/lib/aursmith
ENTRYPOINT ["/usr/local/bin/aursmith-worker"]
ARG AURSMITH_SOURCE_GIT_COMMIT
LABEL org.opencontainers.image.revision=$AURSMITH_SOURCE_GIT_COMMIT
