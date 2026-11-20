//! A working-tree file as the index would store it.

use super::*;

impl NativeRepo<'_> {
    /// Working-tree `content` of `path` as it is stored, when that differs.
    /// `index` is where an `auto` path's staged content is looked up. A
    /// failure is left with the converter: see [`Converter::check`].
    pub(in crate::native) fn stored_form(
        &self,
        converter: &Converter,
        index: &Index,
        path: &[u8],
        content: &[u8],
    ) -> Option<Vec<u8>> {
        // the index is ordered by path, then stage
        let from = index.entries.partition_point(|e| e.path.as_slice() < path);
        let staged = index.entries[from..]
            .iter()
            .take_while(|e| e.path.as_slice() == path)
            .find(|e| e.stage() == 0);
        self.stored_form_of(converter, staged.map(|e| e.oid), path, content)
    }

    /// [`stored_form`](Self::stored_form) for a path whose staged blob,
    /// if it has one, is `staged`.
    pub(in crate::native) fn stored_form_of(
        &self,
        converter: &Converter,
        staged: Option<ObjectId>,
        path: &[u8],
        content: &[u8],
    ) -> Option<Vec<u8>> {
        converter.to_store(
            path,
            content,
            &|| match staged.map(|oid| self.blob_bytes(oid)) {
                Some(Ok(blob)) => attributes::stored_with_crlf(&blob),
                Some(Err(e)) => {
                    converter.fail(e);
                    false
                }
                None => false,
            },
        )
    }
}
