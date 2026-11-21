//! The prisms every object database is opened with.

use alt_prism::Registry;

/// The production prism set, registered into every freshly opened
/// [`NativeOdb`]. Centralised here so a reader and a writer always agree on
/// which prisms a stored Tier 1 record might invoke — adding a prism to
/// production is a single edit, and stores opened by any tool recompose the
/// same blobs.
///
/// Order is priority (`Registry::register` is hot-first). [`alt_prism_deflate::DeflatePrism`]
/// is currently the only entry: the "universal key", since most binary asset containers wrap deflate streams that defeat
/// CDC dedup otherwise.
pub fn default_registry() -> Registry {
    let mut r = Registry::new();
    // Hot-first registration order. Signatures are disjoint (PK / 89PNG
    // / zlib magic), so any order works for correctness; this one
    // is hot-first.
    r.register(Box::new(alt_prism_zip::ZipPrism));
    r.register(Box::new(alt_prism_png::PngPrism));
    r.register(Box::new(alt_prism_deflate::DeflatePrism));
    r
}
