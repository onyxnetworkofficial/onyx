# syntax=docker/dockerfile:1@sha256:4edf897a3ffa55b89f906fc8cc78afdb3f1834cc9c7083565e611a8a7d5fe99e
# onyx-node: the onyx-api service (docs/ops/deployment.md).
#
#   docker build -t onyx-node:local .
#   docker build --target onyx-node-btc -t onyx-node-btc:local .   # with the
#     Bitcoin serving check (btc.entity-reserve, ADR-0034 Amendment 1) and
#     the committed fixtures it reads (/usr/share/onyx; review RESAPI-R1 F4)
#   docker run --read-only --rm --stop-timeout 75 -p 127.0.0.1:8080:8080 \
#     --env-file node.env -v onyx-state:/var/lib/onyx/state \
#     -v /etc/onyx:/etc/onyx:ro onyx-node:local
#
# --stop-timeout 75: Docker's default (10 s) SIGKILLs the server while settles
# that already reached the chain are still writing their receipts (OPS-L1-F1).
# A Dockerfile cannot set it; every `docker run` must.
#
# Base images are pinned by digest; bump them on purpose (docs/ops/upgrades.md).
# So is the syntax line above: BuildKit pulls that frontend image and runs it
# to parse this file, so a moving `docker/dockerfile:1` tag would change the
# build (L7-BK-1, docs/audit/2026-10-15-fresh-install.md). The classic
# builder ignores it.
# The build context is an allowlist (.dockerignore): no .env, no keys.

FROM rust:1.98.0-slim-bookworm@sha256:1469a27c125cb5a3aebfa4f4e4665d935b02fb72cc093b2c974b3d740e43f157 AS build
WORKDIR /src
COPY . .
# rust-toolchain.toml is not in the context: the image is already 1.98.0, and
# the server needs neither the wasm target nor clippy.
RUN cargo build --release --locked -p onyx-node \
 && install -D -m 0755 target/release/onyx-node /out/usr/local/bin/onyx-node \
 && install -d -m 0700 -o 65532 -g 65532 /out/var/lib/onyx/state

# onyx-node-btc: the same server with btc.entity-reserve's serving check. It
# reads the committed scan, label set, cross-checks and header chain named by
# deploy/reserve-gate.json, each checked against the fixtures' SHA256SUMS
# (only their lines are shipped). Mount another config and set
# ONYX_RESERVE_GATE to change the served chain's start or add second scans.
FROM rust:1.98.0-slim-bookworm@sha256:1469a27c125cb5a3aebfa4f4e4665d935b02fb72cc093b2c974b3d740e43f157 AS build-btc
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p onyx-reserve-serve --bin onyx-node-btc \
 && install -D -m 0755 target/release/onyx-node-btc /out/usr/local/bin/onyx-node-btc \
 && install -d -m 0700 -o 65532 -g 65532 /out/var/lib/onyx/state \
 && for f in reserves/scan-971199.json reserves/cross-checks.json labels/test.onyx.entities.btc.v2.json reserves/headers-969839-971207.json; do \
      install -D -m 0644 "fixtures/$f" "/out/usr/share/onyx/fixtures/$f" \
      && grep -F "  $f" fixtures/SHA256SUMS >> /out/usr/share/onyx/fixtures/SHA256SUMS || exit 1; \
    done \
 && install -D -m 0644 deploy/reserve-gate.json /out/usr/share/onyx/deploy/reserve-gate.json

FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f AS onyx-node-btc
COPY --from=build-btc /out/ /
USER 65532:65532
ENV ONYX_LISTEN=0.0.0.0:8080 \
    ONYX_STATE_DIR=/var/lib/onyx/state \
    ONYX_RESERVE_GATE=/usr/share/onyx/deploy/reserve-gate.json
VOLUME ["/var/lib/onyx/state"]
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=60s --retries=3 \
  CMD ["/usr/local/bin/onyx-node-btc", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/onyx-node-btc"]

# onyx-node (the default target: last in this file).
FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f AS onyx-node
COPY --from=build /out/ /
USER 65532:65532
ENV ONYX_LISTEN=0.0.0.0:8080 \
    ONYX_STATE_DIR=/var/lib/onyx/state
# Receipt journals and credits in flight (REV-11): keep this on a volume and
# back it up; losing it loses unposted receipts.
VOLUME ["/var/lib/onyx/state"]
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=60s --retries=3 \
  CMD ["/usr/local/bin/onyx-node", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/onyx-node"]
