# csm-relay image. Build from the repository root:
#
#   docker build -f relay/Dockerfile -t csm-relay .
#
# The relay needs only the protocol crate, so the build stage trims the workspace to
# `crates/csm-remote-proto` + `relay` (the rest of the repo is the desktop app). Cargo.lock is
# copied, so the relay's dependencies keep the versions the repo tests with.

FROM rust:1.97-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates/csm-remote-proto crates/csm-remote-proto
COPY relay relay
RUN sed -i 's|^members = .*|members = ["crates/csm-remote-proto", "relay"]|; /^\[patch\./,$d' Cargo.toml \
 && cargo build --release -p csm-relay \
 && mkdir /data && chown 65532:65532 /data

# Non-root, no shell, no package manager.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/target/release/csm-relay /usr/local/bin/csm-relay
# One writable volume: the VAPID key pair (created on first run). Everything else is in memory.
COPY --from=build --chown=65532:65532 /data /data
VOLUME /data
ENV CSM_RELAY_BIND=0.0.0.0:8787 \
    CSM_RELAY_VAPID_KEY_FILE=/data/vapid.key
EXPOSE 8787
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s CMD ["/usr/local/bin/csm-relay", "healthcheck"]
USER nonroot
ENTRYPOINT ["/usr/local/bin/csm-relay"]
