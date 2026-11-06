# Build only from this checkout. No branch clone or mutable image tag lookup.
FROM --platform=$BUILDPLATFORM oven/bun:1.4.4-debian@sha256:93d5032fe06d163427822dc1598f420363759bb18155c9cc90eb1be05a64a19e AS web
WORKDIR /build/workbench
COPY workbench/package.json workbench/bun.lock ./
COPY workbench/vendor ./vendor
RUN bun install --frozen-lockfile
COPY workbench/ ./
RUN bun run build && rm -f dist/assets/*.map

FROM --platform=$BUILDPLATFORM rust:1.99.1-bookworm@sha256:7085845219c18e7c5b1f34ed9ce89b84f822f2e3d54a02774cedc996ac0f304f AS build
ARG TARGETARCH
ARG ALT_BUILD_ID
ARG CARGO_BUILD_JOBS=2
ENV CARGO_BUILD_JOBS=$CARGO_BUILD_JOBS
WORKDIR /build
RUN apt-get update && apt-get install -y --no-install-recommends gcc-aarch64-linux-gnu gcc-x86-64-linux-gnu libc6-dev-arm64-cross libc6-dev-amd64-cross && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY crates/ ./crates/
COPY vendor/ ./vendor/
RUN --mount=type=cache,id=alt-server-registry,target=/usr/local/cargo/registry,sharing=locked --mount=type=cache,id=alt-server-target-${TARGETARCH},target=/build/target,sharing=locked test -n "$ALT_BUILD_ID" && case "$TARGETARCH" in amd64) target=x86_64-unknown-linux-gnu;; arm64) target=aarch64-unknown-linux-gnu;; *) exit 2;; esac && \
    rustup target add "$target" && \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc \
    ALT_BUILD_ID="$ALT_BUILD_ID" cargo build --release --locked --target "$target" -p alt-server --bin alt-server && \
    mkdir /out && cp "target/$target/release/alt-server" /out/alt-server
RUN case "$TARGETARCH" in \
      amd64) digest=cbe24006683f8eb669266162894b9a522a1af52f2665fbc63a4bb032ed26ac10;; \
      arm64) digest=6b8dc4333c53a5a57c9e5834e3a48f92605d7154014cd07269ff3327db5d37f4;; *) exit 2;; esac && \
    curl -fsSL "https://github.com/FiloSottile/age/releases/download/v1.3.2/age-v1.3.2-linux-$TARGETARCH.tar.gz" -o /tmp/age.tar.gz && \
    printf '%s  /tmp/age.tar.gz\n' "$digest" | sha256sum -c - && \
    tar -xzf /tmp/age.tar.gz -C /out age/age age/age-keygen

# Same PostgreSQL major/client version as the supported database image.
FROM postgres:18.6-bookworm@sha256:f1ea2a1a4fb7b68d4836be1fdcb8a6466a89b072158bf9d97663600538c294c5
ARG ALT_BUILD_ID
LABEL org.opencontainers.image.title="alt-server" org.opencontainers.image.revision="$ALT_BUILD_ID"
COPY --from=build /out/alt-server /usr/local/bin/alt-server
COPY --from=build /out/age/age /out/age/age-keygen /usr/local/bin/
COPY --from=web /build/workbench/dist /opt/alt/workbench
COPY self-hosted/entrypoint.sh /usr/local/bin/alt-entrypoint
COPY LICENSE-MIT LICENSE-APACHE /opt/alt/licenses/
RUN mkdir -p /var/lib/alt-cloud && chown 9000:9000 /var/lib/alt-cloud && chmod 0700 /var/lib/alt-cloud
ENV TMPDIR=/tmp HOME=/tmp
USER 9000:9000
WORKDIR /opt/alt
ENTRYPOINT ["/usr/local/bin/alt-entrypoint"]
CMD ["serve"]
