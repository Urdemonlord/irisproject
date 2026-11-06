# syntax=docker/dockerfile:1
# IRIS web — build the Vite bundle, serve it with nginx, proxy /api + /auth
# to the API service. Build context is the repo root
# (see deploy/compose/docker-compose.yaml).
FROM node:22-bookworm-slim AS builder
WORKDIR /app
COPY apps/web/package.json apps/web/package-lock.json ./
RUN npm ci
COPY apps/web/ ./
# Build-time public site key for the Turnstile login widget (empty =
# no widget). Passed as a build arg so the secret never enters the
# image; see deploy/compose/docker-compose.yaml for the wiring.
ARG VITE_TURNSTILE_SITE_KEY
ENV VITE_TURNSTILE_SITE_KEY=$VITE_TURNSTILE_SITE_KEY
RUN npm run build

FROM nginx:alpine
COPY deploy/nginx/iris-headers.conf /etc/nginx/snippets/iris-headers.conf
COPY deploy/web-nginx.conf /etc/nginx/conf.d/default.conf
COPY --from=builder /app/dist /usr/share/nginx/html
EXPOSE 80
