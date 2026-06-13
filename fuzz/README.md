# alt fuzz targets

Adversarial-input fuzzing for the parser stones. A standalone crate (its own
`[workspace]`) so the stable `cargo build --workspace` never tries to build it
— it needs nightly + libFuzzer.

## Targets

- `codec_parse` — git commit/tree/tag object parsers on arbitrary bytes.
- `pack_delta_apply` — git delta resolution against a base (untrusted pack).
- `store_delta_decode` — lineage delta (zstd ref-prefix) decode on corrupt input.

## Run

```sh
rustup component add rust-src --toolchain nightly   # once
# the native target is required on Apple Silicon (cargo-fuzz defaults to x86_64)
cargo +nightly fuzz run --target aarch64-apple-darwin <target> -- -max_total_time=60
```

`fuzz/corpus/` and `fuzz/artifacts/` are gitignored (generated). A crash writes
a reproducer under `fuzz/artifacts/<target>/`; replay it by passing the path as
the last argument to `cargo fuzz run`.
