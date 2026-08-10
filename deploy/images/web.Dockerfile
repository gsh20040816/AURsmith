ARG AURSMITH_SOURCE_GIT_COMMIT=unknown
FROM node:22.22.2-bookworm-slim@sha256:9f6d5975c7dca860947d3915877f85607946403fc55349f39b4bc3688448bb6e AS builder
WORKDIR /src
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web ./
RUN npm run build

FROM caddy:2.10.2-alpine@sha256:4c6e91c6ed0e2fa03efd5b44747b625fec79bc9cd06ac5235a779726618e530d
ARG AURSMITH_SOURCE_GIT_COMMIT
COPY deploy/controller/Caddyfile /etc/caddy/Caddyfile
COPY --from=builder /src/dist /srv
LABEL org.opencontainers.image.revision=$AURSMITH_SOURCE_GIT_COMMIT
