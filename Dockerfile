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
ENV DEVHUB_DATA=/data DEVHUB_LISTEN=0.0.0.0:8080
VOLUME /data
CMD ["devhub"]
