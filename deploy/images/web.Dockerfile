FROM node:22.22.0-bookworm-slim AS builder
WORKDIR /src
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web ./
RUN npm run build

FROM caddy:2.10.2-alpine
COPY deploy/controller/Caddyfile /etc/caddy/Caddyfile
COPY --from=builder /src/dist /srv
