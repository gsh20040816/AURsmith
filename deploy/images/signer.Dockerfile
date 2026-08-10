ARG AURSMITH_SOURCE_GIT_COMMIT=unknown
FROM rust:1.97.1-bookworm@sha256:14bc9c5966e7b3a385794b3d5389a8765668342025fbcc7b2e3d2866ac4bd8c3 AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY migrations ./migrations
COPY crates ./crates
RUN cargo build --locked --release -p aursmith-signer

FROM archlinux:base@sha256:345a872f6c95e082d4b8c050af637eebb57402c6e2177b411c3acf7df84eb33b
ARG AURSMITH_SOURCE_GIT_COMMIT
RUN pacman -Syu --noconfirm --needed gnupg libarchive pacman \
    && rm -rf /var/cache/pacman/pkg/* /var/lib/pacman/sync/* \
    && useradd --uid 10001 --create-home --home-dir /var/lib/aursmith-signer --shell /usr/bin/nologin signer \
    && install -d -o signer -g signer /inbox /signed
COPY --from=builder /src/target/release/aursmith-signer /usr/local/bin/aursmith-signer
USER 10001:10001
WORKDIR /var/lib/aursmith-signer
ENTRYPOINT ["/usr/local/bin/aursmith-signer"]
LABEL org.opencontainers.image.revision=$AURSMITH_SOURCE_GIT_COMMIT
