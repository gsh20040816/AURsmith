ARG AURSMITH_SOURCE_GIT_COMMIT=unknown
FROM archlinux:base@sha256:345a872f6c95e082d4b8c050af637eebb57402c6e2177b411c3acf7df84eb33b
ARG AURSMITH_SOURCE_GIT_COMMIT

RUN pacman -Syu --noconfirm --needed squid ca-certificates \
    && rm -rf /var/cache/pacman/pkg/* /var/lib/pacman/sync/* \
    && install -d -o proxy -g proxy /run/squid /var/log/squid

COPY deploy/publisher/squid.conf /etc/squid/squid.conf

USER proxy:proxy
ENTRYPOINT ["/usr/bin/squid", "-N", "-f", "/etc/squid/squid.conf"]
LABEL org.opencontainers.image.revision=$AURSMITH_SOURCE_GIT_COMMIT
