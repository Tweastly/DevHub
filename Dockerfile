FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/devhub /usr/local/bin/devhub
ENV DEVHUB_DATA=/data DEVHUB_LISTEN=0.0.0.0:8080
VOLUME /data
CMD ["devhub"]
