#!/bin/bash
# Builds alt-server in a target directory that is kept between builds.
# usage: build-server.sh <target triple> <output file>, with ALT_BUILD_ID set
# to what names the sources in the working directory.
set -euo pipefail
target=$1
output=$2
test -n "$ALT_BUILD_ID"

# cargo takes a crate for built where its files are older than what was built
# of it. The files here have the times of the checkout, or of the build that
# first copied this very content, and the directory may have been built from
# other sources since: they are given the time of this build unless the
# directory was last built from these sources.
mkdir -p target
other=
if [ "$(cat target/.built-from 2>/dev/null)" != "$ALT_BUILD_ID" ]; then
    other=1
    find Cargo.toml Cargo.lock crates vendor -type f -exec touch {} +
fi
cargo build -v --release --locked --target "$target" -p alt-server --bin alt-server 2>&1 | tee target/.build-log
# what cargo did not compile it took as built already: after other sources,
# that is a crate of those sources in the binary
if [ -n "$other" ] && grep -E "^ +Fresh [^ ]+ v[^ ]+ \(/" target/.build-log; then
    echo "build-server: the crates above were not built from these sources" >&2
    exit 1
fi
printf '%s' "$ALT_BUILD_ID" > target/.built-from
cp "target/$target/release/alt-server" "$output"
