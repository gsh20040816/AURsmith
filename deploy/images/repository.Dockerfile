ARG AURSMITH_SOURCE_GIT_COMMIT=unknown
FROM caddy:2.10.2-alpine@sha256:4c6e91c6ed0e2fa03efd5b44747b625fec79bc9cd06ac5235a779726618e530d
ARG AURSMITH_SOURCE_GIT_COMMIT
RUN setcap -r /usr/bin/caddy \
    && addgroup -g 10001 -S aursmith \
    && adduser -u 10001 -S -D -H -G aursmith aursmith
USER 10001:10001
LABEL org.opencontainers.image.revision=$AURSMITH_SOURCE_GIT_COMMIT
