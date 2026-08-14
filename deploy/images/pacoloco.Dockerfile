FROM ghcr.io/anatol/pacoloco@sha256:c5e7c82e6081e1edfd95f8cac8dd9e5e029b75557a63019c2e5034f35dd6623a

ARG AURSMITH_SOURCE_GIT_COMMIT=unknown
ARG AURSMITH_ARCH_MIRROR=https://mirrors.ustc.edu.cn/archlinux
USER 0:0
RUN case "${AURSMITH_ARCH_MIRROR}" in https://*) ;; *) echo 'AURSMITH_ARCH_MIRROR 必须是 HTTPS URL' >&2; exit 1 ;; esac \
    && case "${AURSMITH_ARCH_MIRROR}" in *[[:space:]]*|*'@'*|*'?'*|*'#'*) echo 'AURSMITH_ARCH_MIRROR 不能包含空白、凭据、查询参数或片段' >&2; exit 1 ;; esac \
    && install -d -o 65532 -g 65532 /var/cache/pacoloco \
    && printf 'address: 0.0.0.0\nport: 9129\ncache_dir: /var/cache/pacoloco\npurge_files_after: 0\ndownload_timeout: 3600\nrepos:\n  archlinux:\n    urls:\n      - %s\n' "${AURSMITH_ARCH_MIRROR%/}" > /etc/pacoloco.yaml \
    && chmod 0444 /etc/pacoloco.yaml

USER 65532:65532
LABEL org.opencontainers.image.revision=$AURSMITH_SOURCE_GIT_COMMIT
