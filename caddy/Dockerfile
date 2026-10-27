# Caddy with the Cloudflare DNS module: lets it get one wildcard certificate for
# *.<DEVHUB_DOMAIN> (DNS challenge), used when DEVHUB_TLS=wildcard.
FROM caddy:2-builder AS build
RUN xcaddy build --with github.com/caddy-dns/cloudflare

FROM caddy:2
COPY --from=build /usr/bin/caddy /usr/bin/caddy
