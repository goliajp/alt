# alt-byte-segment

Bounded immutable containers of original content-addressed bytes. The library
encodes deterministic members, validates the index and file extent, exposes
bounded member readers, and separately verifies all payloads and checksums.

Limits are 8,192 distinct contents and 64 MiB of original bytes per segment.
The versioned header includes a checked index of BLAKE3 identities, offsets,
and lengths. Payloads remain uncompressed and byte-for-byte unchanged.

This crate does not authorize access, install files, synchronize storage, or
publish database references. Successful encoding is not a durability proof.
Opening checks the index; call `verify` to verify the complete contents.
