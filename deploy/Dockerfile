# AURsmith 的全部镜像都从这一个 Dockerfile 的不同 target 构建：
#
#   aursmithd  公网主服务（Web/API、AUR 同步、2+1 审查、调度、发布计划）
#   signer     无网络签名器（同一个 aursmithd 二进制的 signer 子命令）
#   builder    家用 Builder 守护进程（只带 docker CLI）
#   build      一次性构建容器（Arch + base-devel，入口为 aursmith-builder guest）
ARG AURSMITH_SOURCE_GIT_COMMIT=unknown

FROM node:22.22.2-bookworm-slim@sha256:9f6d5975c7dca860947d3915877f85607946403fc55349f39b4bc3688448bb6e AS web
WORKDIR /src
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web ./
RUN npm run build

FROM rust:1.97.1-bookworm@sha256:14bc9c5966e7b3a385794b3d5389a8765668342025fbcc7b2e3d2866ac4bd8c3 AS rust
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --locked --release -p aursmithd -p aursmith-builder

FROM debian:bookworm-slim@sha256:abd67ffcfa541b485a3dff59865ab629aa048a6c613e639d36e7456b0b229241 AS aursmithd
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates git \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home --home-dir /var/lib/aursmith --shell /usr/sbin/nologin aursmith \
    && install -d -o aursmith -g aursmith /var/lib/aursmith-exchange
COPY --from=rust /src/target/release/aursmithd /usr/local/bin/aursmithd
COPY --from=web /src/dist /srv/web
USER 10001:10001
WORKDIR /var/lib/aursmith
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/aursmithd"]
CMD ["serve"]
ARG AURSMITH_SOURCE_GIT_COMMIT
LABEL org.opencontainers.image.revision=$AURSMITH_SOURCE_GIT_COMMIT

# 签名器需要 gpg、repo-add、makepkg（构建 aursmith-keyring）、bsdtar、fakeroot、zstd。
FROM archlinux:base@sha256:345a872f6c95e082d4b8c050af637eebb57402c6e2177b411c3acf7df84eb33b AS signer
RUN pacman -Syu --noconfirm --needed binutils fakeroot gnupg libarchive pacman zstd \
    && rm -rf /var/cache/pacman/pkg/* /var/lib/pacman/sync/* \
    && useradd --uid 10001 --create-home --home-dir /var/lib/aursmith --shell /usr/sbin/nologin aursmith \
    && install -d -o aursmith -g aursmith /var/lib/aursmith-exchange /srv/repo
COPY --from=rust /src/target/release/aursmithd /usr/local/bin/aursmithd
USER 10001:10001
WORKDIR /var/lib/aursmith
ENTRYPOINT ["/usr/local/bin/aursmithd", "signer"]
ARG AURSMITH_SOURCE_GIT_COMMIT
LABEL org.opencontainers.image.revision=$AURSMITH_SOURCE_GIT_COMMIT

FROM docker:28-cli@sha256:625d9431a9f54c5a2bc90f24f0e1c3d55b1349fd857dd85035f98c2c9acbdd4d AS docker-cli

FROM debian:bookworm-slim@sha256:abd67ffcfa541b485a3dff59865ab629aa048a6c613e639d36e7456b0b229241 AS builder
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home --home-dir /var/lib/aursmith-builder --shell /usr/sbin/nologin aursmith
COPY --from=docker-cli /usr/local/bin/docker /usr/bin/docker
COPY --from=rust /src/target/release/aursmith-builder /usr/local/bin/aursmith-builder
USER 10001:10001
WORKDIR /var/lib/aursmith-builder
ENTRYPOINT ["/usr/local/bin/aursmith-builder"]
CMD ["run"]
ARG AURSMITH_SOURCE_GIT_COMMIT
LABEL org.opencontainers.image.revision=$AURSMITH_SOURCE_GIT_COMMIT

FROM archlinux:base@sha256:b860afd5823683f7ea389ba5f00d812f4fe55f6f286dea329d2abeefa535e309 AS build
ARG AURSMITH_ARCH_MIRROR=https://mirrors.ustc.edu.cn/archlinux
ARG AURSMITH_ARCHLINUXCN_MIRROR=https://mirrors.ustc.edu.cn/archlinuxcn
COPY deploy/common/pacman-aursmith.conf /etc/pacman.conf
RUN for mirror in "${AURSMITH_ARCH_MIRROR}" "${AURSMITH_ARCHLINUXCN_MIRROR}"; do \
      case "${mirror}" in https://*) ;; *) echo '镜像地址必须是 HTTPS URL' >&2; exit 1 ;; esac; \
      case "${mirror}" in *[[:space:]]*|*'@'*|*'?'*|*'#'*) echo '镜像地址不能包含空白、凭据、查询参数或片段' >&2; exit 1 ;; esac; \
    done \
    && printf 'Server = %s/$repo/os/$arch\n' "${AURSMITH_ARCH_MIRROR%/}" > /etc/pacman.d/mirrorlist \
    && printf 'Server = %s/$arch\n' "${AURSMITH_ARCHLINUXCN_MIRROR%/}" > /etc/pacman.d/archlinuxcn-mirrorlist \
    && pacman-key --init \
    && pacman-key --populate archlinux \
    && pacman -Sy --noconfirm --needed archlinux-keyring archlinuxcn-keyring \
    && pacman-key --populate archlinux archlinuxcn \
    && pacman -Syu --noconfirm --needed base-devel ca-certificates git gnupg sudo \
    && rm -rf /var/cache/pacman/pkg/* \
    && useradd --uid 1000 --create-home --shell /bin/bash builder \
    && printf 'builder ALL=(root) NOPASSWD: /usr/bin/pacman\n' > /etc/sudoers.d/aursmith-builder \
    && chmod 0440 /etc/sudoers.d/aursmith-builder \
    && install -d -o builder -g builder /build /mnt/aursmith-input /mnt/aursmith-output
COPY deploy/common/makepkg-aursmith.conf /etc/aursmith/makepkg.conf
COPY --from=rust /src/target/release/aursmith-builder /usr/local/bin/aursmith-builder
ENTRYPOINT ["/usr/local/bin/aursmith-builder", "guest"]
ARG AURSMITH_SOURCE_GIT_COMMIT
LABEL org.opencontainers.image.revision=$AURSMITH_SOURCE_GIT_COMMIT
