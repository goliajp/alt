# alt server — pure-Rust git smart-http v2 wire + browse JSON API for
# alt.golia.jp. Live path on t01: /apps/alt/Dockerfile (rsynced from
# deploy/ in this repo).
#
# Builder (rust:1.88-bookworm) clones goliajp/alt and builds the three
# release binaries; the runtime layer (debian:bookworm-slim) carries
# only those binaries plus a stable system user. Repository data lives
# under `/var/lib/alt-repos` and is supplied by the compose host via a
# bind mount — the image stays a pure binary distribution and
# the host owns the actual content.
#
# Update flow:
#   1. on a workstation: `scripts/deploy-repos.sh`
#      → populate /apps/alt/repos/{alt,demo-binaries}/.alt on t01.
#   2. on t01:        `docker compose build --pull && docker compose up -d`
# A cache-bust ADD ahead of `git clone` makes step 2 pick up new
# develop commits without --no-cache.

FROM rust:1.88-bookworm AS builder
RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src

# Cache-bust: GitHub's refs API returns the current develop tip's SHA,
# so its content changes whenever develop moves.
ADD https://api.github.com/repos/goliajp/alt/git/refs/heads/develop /tmp/develop-ref.json
RUN git clone https://github.com/goliajp/alt.git .

# Three binaries: `alt` (still useful inside the image for debugging
# imports), `altd-server` (wire), `alt-web` (browse JSON API).
RUN cargo build --release \
    --bin alt -p alt-cli \
    --bin altd-server -p alt-cli \
    --bin alt-web -p alt-web

# ──────────────────────────── runtime ─────────────────────────────
FROM debian:bookworm-slim AS runtime-base
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates wget \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 9000 --home /var/lib/alt-repos --shell /usr/sbin/nologin altd \
    && mkdir -p /var/lib/alt-repos \
    && chown altd:altd /var/lib/alt-repos

COPY --from=builder /src/target/release/alt /usr/local/bin/alt
COPY --from=builder /src/target/release/altd-server /usr/local/bin/altd-server
COPY --from=builder /src/target/release/alt-web /usr/local/bin/alt-web

ENV ALT_SERVER_ROOT=/var/lib/alt-repos
ENV ALT_WEB_ROOT=/var/lib/alt-repos
ENV ALT_SERVER_WORKERS=4
ENV ALT_WEB_WORKERS=2
USER altd
WORKDIR /var/lib/alt-repos

# Default to wire; alt-web service overrides ENTRYPOINT in compose.
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/altd-server", "--bind", "0.0.0.0:8080"]
