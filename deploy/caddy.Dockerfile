# Builds the web app and serves it with Caddy (automatic HTTPS). Build context: repository root.
FROM node:22-alpine AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web/ ./
RUN npm test && npm run build

FROM caddy:2
ARG CADDYFILE=deploy/Caddyfile
COPY --from=web /web/dist /srv
COPY ${CADDYFILE} /etc/caddy/Caddyfile
