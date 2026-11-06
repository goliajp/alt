# alt-provenance

Pure verification and signing of publication evidence. Original receipts remain
unchanged across migrations and recovery. A migration carries source-signed
export inclusion and target-signed acceptance; recovery retains the same key
and binds an advanced epoch to a particular backup.

Callers supply the authenticated current scope and pinned public key. Keys in
downloaded evidence never establish trust by themselves. This crate performs no
I/O and grants no content access or permission to export, import or recover.
