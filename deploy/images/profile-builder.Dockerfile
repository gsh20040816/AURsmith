ARG AURSMITH_SOURCE_GIT_COMMIT=unknown
FROM rust:1.97.1-bookworm@sha256:14bc9c5966e7b3a385794b3d5389a8765668342025fbcc7b2e3d2866ac4bd8c3 AS rust-builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY migrations ./migrations
COPY crates ./crates
RUN cargo build --locked --release -p aursmith-guest-agent -p aursmithctl

FROM archlinux:base@sha256:345a872f6c95e082d4b8c050af637eebb57402c6e2177b411c3acf7df84eb33b AS profile
ARG AURSMITH_ARCH_MIRROR=https://mirrors.ustc.edu.cn/archlinux
COPY deploy/common/pacman-aursmith.conf /etc/pacman.conf
RUN case "${AURSMITH_ARCH_MIRROR}" in https://*) ;; *) echo 'AURSMITH_ARCH_MIRROR 必须是 HTTPS URL' >&2; exit 1 ;; esac \
    && case "${AURSMITH_ARCH_MIRROR}" in *[[:space:]]*) echo 'AURSMITH_ARCH_MIRROR 不能包含空白' >&2; exit 1 ;; esac \
    && case "${AURSMITH_ARCH_MIRROR}" in *'@'*|*'?'*|*'#'*) echo 'AURSMITH_ARCH_MIRROR 不能包含凭据、查询参数或片段' >&2; exit 1 ;; esac \
    && repository_mirror="${AURSMITH_ARCH_MIRROR%/}" \
    && printf 'Server = %s/$repo/os/$arch\n' "${repository_mirror}" > /etc/pacman.d/mirrorlist \
    && pacman -Syu --noconfirm --needed e2fsprogs qemu-img mkinitcpio \
    && install -d /rootfs/var/lib/pacman /rootfs/etc /opt/aursmith-profile \
    && pacman -Sy --noconfirm --root /rootfs --dbpath /rootfs/var/lib/pacman \
      --cachedir /var/cache/pacman/pkg base linux base-devel devtools \
    && install -Dm644 /etc/pacman.d/mirrorlist /rootfs/etc/pacman.d/mirrorlist \
    && printf '%s\n' "${repository_mirror}" > /opt/aursmith-profile/repository-mirror \
    && useradd --root /rootfs --uid 1000 --create-home --shell /bin/bash builder
RUN install -Dm644 /etc/pacman.conf /rootfs/etc/pacman.conf
RUN pacman-key --gpgdir /rootfs/etc/pacman.d/gnupg --init \
    && pacman-key --gpgdir /rootfs/etc/pacman.d/gnupg --populate archlinux
COPY deploy/common/mkinitcpio-aursmith.conf /rootfs/etc/mkinitcpio.conf
COPY deploy/common/makepkg-aursmith.conf /rootfs/etc/aursmith/makepkg.conf
COPY deploy/common/aursmith-guest-agent.service /rootfs/usr/lib/systemd/system/aursmith-guest-agent.service
COPY --from=rust-builder /src/target/release/aursmith-guest-agent /rootfs/usr/local/bin/aursmith-guest-agent
RUN chmod 0755 /rootfs/usr/local/bin/aursmith-guest-agent \
    && systemctl --root=/rootfs enable aursmith-guest-agent.service \
    && chown -R 1000:1000 /rootfs/home/builder \
    && kernel_version="$(basename "$(find /rootfs/usr/lib/modules -mindepth 1 -maxdepth 1 -type d -print -quit)")" \
    && test -n "${kernel_version}" \
    && mkinitcpio -r /rootfs -c /rootfs/etc/mkinitcpio.conf -k "${kernel_version}" -g /rootfs/boot/initramfs-linux.img \
    && cp "/rootfs/usr/lib/modules/${kernel_version}/vmlinuz" /opt/aursmith-profile/vmlinuz-linux \
    && cp /rootfs/boot/initramfs-linux.img /opt/aursmith-profile/initramfs-linux.img \
    && pacman --root /rootfs --dbpath /rootfs/var/lib/pacman -Q > /opt/aursmith-profile/installed-packages.txt \
    && date --utc --iso-8601=seconds > /opt/aursmith-profile/created-at \
    && truncate -s 16G /tmp/root.raw \
    && mkfs.ext4 -F -d /rootfs /tmp/root.raw \
    && qemu-img convert -f raw -O qcow2 -c /tmp/root.raw /opt/aursmith-profile/root.qcow2 \
    && rm -f /tmp/root.raw \
    && chown -R 10001:10001 /opt/aursmith-profile

FROM archlinux:base@sha256:345a872f6c95e082d4b8c050af637eebb57402c6e2177b411c3acf7df84eb33b
RUN useradd --uid 10001 --create-home --home-dir /var/lib/aursmith-profile profile \
    && install -d -o profile -g profile /out
COPY --from=rust-builder /src/target/release/aursmithctl /usr/local/bin/aursmithctl
COPY --from=profile --chown=10001:10001 /opt/aursmith-profile /opt/aursmith-profile
USER 10001:10001
ENTRYPOINT ["/usr/local/bin/aursmithctl", "export-profile"]
ARG AURSMITH_SOURCE_GIT_COMMIT
LABEL org.opencontainers.image.revision=$AURSMITH_SOURCE_GIT_COMMIT
