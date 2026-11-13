# torajs-encoding

The encodings of the [WHATWG Encoding Standard](https://encoding.spec.whatwg.org/):

- `Encoding`: every encoding of the standard's table, its name, and
  `Encoding::for_label`, the standard's "get an encoding" (ASCII
  whitespace trimmed, ASCII case folded).
- Decoders to UTF-8 for the legacy single-byte encodings (`single_byte`,
  with the standard's indexes as static tables), for `x-user-defined`, and
  for UTF-16LE / UTF-16BE (`utf16`, resumable across chunks).
- Decoders to UTF-8 for the legacy multi-byte encodings, each resumable
  across chunks: GBK and gb18030 (`gb18030`), Big5 (`big5`), EUC-JP
  (`euc_jp`), ISO-2022-JP (`iso_2022_jp`), Shift_JIS (`shift_jis`) and
  EUC-KR (`euc_kr`).

Each decoder reports an error where the standard's decoder returns error
when asked to be fatal, and otherwise writes U+FFFD there and goes on.
Every index is its own static, so a program links only the indexes of the
decoders it calls.

The tables are generated from the standard's `encodings.json` and
`index-<name>.txt` files. `no_std` with `alloc`, no dependencies.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

## Source

Copied from `crates/torajs-encoding` of https://github.com/goliajp/torajs at
commit `c4f101448a338768eeb5d90b3a46a223c21326e2`. `src/`, `tools/` and the
license files are unmodified; `Cargo.toml` states the values the original
inherits from its workspace.
