# alt-command-protocol

Pure encoding and validation for existing whole-command intent, completion and
response receipts. Local CLI request storage and daemon framing use these same
types; source task publication can validate them without depending on the CLI.

The v1 response encoding remains an exit byte followed by two little-endian u32
length-prefixed stdout/stderr streams. Decoding rejects truncation and trailing
bytes. Completion validation binds the original request and command digest to
the exact response digest and length. These records describe source facts;
reading one does not execute its command or authorize replay at another instance.
