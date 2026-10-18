# botstack-bridge: the bridge between the robots and xAI's realtime voice.
# The toolchain CI checks with (.github/workflows/ci.yml pins 1.98.1).
FROM rust:1.98-bookworm AS build
RUN apt-get update \
 && apt-get install -y --no-install-recommends libopus-dev pkg-config cmake \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY bridge bridge
RUN cargo build --release -p botstack-bridge

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends libopus0 ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/botstack-bridge /usr/local/bin/botstack-bridge
# The bridge's state (fleet.json, published firmware). A fresh named volume
# copies this directory's owner, so the unprivileged user can write it.
RUN mkdir /data && chown 65532:65532 /data
VOLUME /data
USER 65532:65532
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/botstack-bridge"]
