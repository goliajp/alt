#!/bin/sh
# regenerates src/tables.rs from the C library of this system, which must
# be the GNU one and have the locale C.UTF-8; the argument is the Unicode
# version that library's locale data follows
# usage: sh crates/alt-regex/tables/gen.sh 16.0.0 > crates/alt-regex/src/tables.rs
set -eu
here=$(dirname "$0")
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
cc -O1 -o "$out/gen" "$here/gen.c"
"$out/gen" C.UTF-8 "$1"
