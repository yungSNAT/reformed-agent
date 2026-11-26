# syntax=docker/dockerfile:1.7@sha256:a57df69d0ea827fb7266491f2813635de6f17269be881f696fbfdf2d83dda33e

# ----- Build stage -----
FROM rust:1.99.1-bookworm@sha256:e6f2cff35d0f309acf8433b4f05878ed77092602c5065cc81801177da0bb3a9c AS builder

WORKDIR /app

# Copy workspace manifests first for better layer caching
COPY --link Cargo.toml Cargo.lock ./
COPY --link crates crates
# The workspace patches four crates from third_party; the build fails without them.
COPY --link third_party third_party

# hermes-cli's default features enable every gateway platform adapter.
# `hermes` is a launcher that runs `hermes-agent-ultra` from its own directory.
RUN cargo build --release --locked -p hermes-cli --bin hermes --bin hermes-agent-ultra -p hermes-entrypoint --bin hermes-entrypoint

# ----- Runtime stage -----
FROM debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc
LABEL org.opencontainers.image.source="https://github.com/yungSNAT/reformed-agent" \
      org.opencontainers.image.licenses="MIT"

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tini gosu \
    && rm -rf /var/lib/apt/lists/*

COPY --link --from=builder --chmod=0755 /app/target/release/hermes /usr/local/bin/hermes
COPY --link --from=builder --chmod=0755 /app/target/release/hermes-agent-ultra /usr/local/bin/hermes-agent-ultra
COPY --link --from=builder --chmod=0755 /app/target/release/hermes-entrypoint /usr/local/bin/hermes-entrypoint
COPY --link LICENSE NOTICE /usr/share/doc/hermes-agent-ultra/
RUN chmod 0644 /usr/share/doc/hermes-agent-ultra/LICENSE /usr/share/doc/hermes-agent-ultra/NOTICE \
    && groupadd -g 10000 hermes \
    && useradd -u 10000 -g 10000 -m -s /bin/sh hermes \
    && install -d -o 10000 -g 10000 -m 0755 /data

ENV HERMES_HOME=/data
VOLUME ["/data"]

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/hermes-entrypoint"]
CMD ["hermes"]
