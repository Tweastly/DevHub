# Caddy, DevHub's HTTPS front, with the Cloudflare DNS module (wildcard certificate,
# DEVHUB_TLS=wildcard). It runs inside DevHub's container, started by DevHub.
FROM caddy:2-builder AS caddy
RUN xcaddy build --with github.com/caddy-dns/cloudflare

FROM rust:1-bookworm AS build
WORKDIR /src
# Dependencies first, in their own layer: it is reused (locally, and from the registry
# cache on GitHub) as long as Cargo.toml and Cargo.lock don't change, so only DevHub
# itself is recompiled.
COPY Cargo.toml Cargo.lock ./
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    mkdir src && echo 'fn main() {}' > src/main.rs && cargo build --release && rm -rf src
COPY src ./src
COPY templates ./templates
COPY assets ./assets
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    touch src/main.rs && cargo build --release && cp target/release/devhub /devhub

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*
COPY --from=build /devhub /usr/local/bin/devhub
COPY --from=caddy /usr/bin/caddy /usr/local/bin/caddy
COPY caddy/Caddyfile caddy/Caddyfile.ip caddy/projects-ondemand.caddy caddy/projects-wildcard.caddy caddy/restarting.html /etc/caddy/
# Caddy keeps its certificates in /caddy (the caddy_data volume of docker-compose.yml).
ENV DEVHUB_DATA=/data DEVHUB_LISTEN=0.0.0.0:8080 XDG_DATA_HOME=/caddy XDG_CONFIG_HOME=/caddy/config
VOLUME /data
# The commit this image was built from and its GitHub repository (passed by GitHub Actions):
# DevHub compares it with GitHub to offer updates, and pulls ghcr.io/<repository>:<commit>.
ARG REVISION=""
ARG REPOSITORY="Tweastly/DevHub"
ENV DEVHUB_REVISION=$REVISION DEVHUB_GITHUB_REPO=$REPOSITORY
CMD ["devhub"]
