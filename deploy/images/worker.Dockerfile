FROM rust:1.97.1-bookworm AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY migrations ./migrations
COPY crates ./crates
RUN cargo build --locked --release -p aursmith-worker -p aursmithctl

FROM archlinux:base-devel
RUN pacman -Syu --noconfirm --needed \
      ca-certificates openssh rsync qemu-full qemu-img virtiofsd \
    && pacman -Scc --noconfirm \
    && useradd --uid 10001 --create-home --home-dir /var/lib/aursmith --shell /usr/bin/nologin aursmith \
    && install -d -o aursmith -g aursmith /run/aursmith /var/lib/aursmith/runtime
COPY --from=builder /src/target/release/aursmith-worker /usr/local/bin/aursmith-worker
COPY --from=builder /src/target/release/aursmithctl /usr/local/bin/aursmithctl
USER 10001:10001
WORKDIR /var/lib/aursmith
ENTRYPOINT ["/usr/local/bin/aursmith-worker"]
