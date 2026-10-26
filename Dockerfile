FROM rust:1-bookworm AS build
WORKDIR /src
# prebuilt/devhub: binary built by GitHub Actions, put there by the updater when it matches
# the checked-out commit. Otherwise compile here; the cache mounts keep the dependencies
# and the target dir between builds, so only DevHub itself is recompiled.
COPY prebuilt ./prebuilt
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY templates ./templates
COPY assets ./assets
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    if [ -x prebuilt/devhub ]; then echo "binaire précompilé (GitHub Actions)" && cp prebuilt/devhub /devhub; \
    else cargo build --release && cp target/release/devhub /devhub; fi

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*
COPY --from=build /devhub /usr/local/bin/devhub
ENV DEVHUB_DATA=/data DEVHUB_LISTEN=0.0.0.0:8080
VOLUME /data
CMD ["devhub"]
