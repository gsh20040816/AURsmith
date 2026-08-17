ARG AURSMITH_SOURCE_GIT_COMMIT=unknown
FROM rust:1.97.1-bookworm@sha256:14bc9c5966e7b3a385794b3d5389a8765668342025fbcc7b2e3d2866ac4bd8c3 AS rust-builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY migrations ./migrations
COPY crates ./crates
RUN cargo build --locked --release -p aursmith-guest-agent

FROM archlinux:base@sha256:345a872f6c95e082d4b8c050af637eebb57402c6e2177b411c3acf7df84eb33b
ARG AURSMITH_ARCH_MIRROR=https://mirrors.ustc.edu.cn/archlinux
ENV DOTNET_CLI_USE_MSBUILD_SERVER=0 \
    MSBUILDDISABLENODEREUSE=1
COPY deploy/common/pacman-aursmith.conf /etc/pacman.conf
RUN case "${AURSMITH_ARCH_MIRROR}" in https://*) ;; *) echo 'AURSMITH_ARCH_MIRROR 必须是 HTTPS URL' >&2; exit 1 ;; esac \
    && case "${AURSMITH_ARCH_MIRROR}" in *[[:space:]]*) echo 'AURSMITH_ARCH_MIRROR 不能包含空白' >&2; exit 1 ;; esac \
    && case "${AURSMITH_ARCH_MIRROR}" in *'@'*|*'?'*|*'#'*) echo 'AURSMITH_ARCH_MIRROR 不能包含凭据、查询参数或片段' >&2; exit 1 ;; esac \
    && repository_mirror="${AURSMITH_ARCH_MIRROR%/}" \
    && printf 'Server = %s/$repo/os/$arch\n' "${repository_mirror}" > /etc/pacman.d/mirrorlist \
    && pacman -Syu --noconfirm --needed base-devel ca-certificates devtools git gnupg namcap sudo \
    && rm -rf /var/cache/pacman/pkg/* \
    && useradd --uid 1000 --create-home --shell /bin/bash builder \
    && printf 'builder ALL=(root) NOPASSWD: /usr/bin/pacman\n' > /etc/sudoers.d/aursmith-builder \
    && chmod 0440 /etc/sudoers.d/aursmith-builder \
    && install -d -o builder -g builder /build /mnt/aursmith-input /mnt/aursmith-output
COPY deploy/common/makepkg-aursmith.conf /etc/aursmith/makepkg.conf
COPY --from=rust-builder /src/target/release/aursmith-guest-agent /usr/local/bin/aursmith-guest-agent
ENTRYPOINT ["/usr/local/bin/aursmith-guest-agent"]
ARG AURSMITH_SOURCE_GIT_COMMIT
LABEL org.opencontainers.image.revision=$AURSMITH_SOURCE_GIT_COMMIT
