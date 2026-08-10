FROM rust:1.97.1-bookworm AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY migrations ./migrations
COPY crates ./crates
RUN cargo build --locked --release -p aursmith-controller -p aursmithctl

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home --home-dir /var/lib/aursmith aursmith
COPY --from=builder /src/target/release/aursmith-controller /usr/local/bin/aursmith-controller
COPY --from=builder /src/target/release/aursmithctl /usr/local/bin/aursmithctl
USER 10001:10001
WORKDIR /var/lib/aursmith
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/aursmith-controller"]
CMD ["serve"]
