//! Line-ending conversion between the object store and the working tree,
//! for the paths `.gitattributes` marks as text.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use alt_worktree::attributes::{self, Conversion, Layer};

use super::*;

/// The `.gitattributes` files of a working tree, each read the first time
/// a path beneath its directory is asked about.
pub(super) struct WorktreeAttributes {
    root: PathBuf,
    layers: RefCell<HashMap<Vec<u8>, Option<Rc<Layer>>>>,
    failed: RefCell<Option<Box<dyn std::error::Error>>>,
}

impl WorktreeAttributes {
    pub(super) fn new(root: &Path) -> Self {
        WorktreeAttributes {
            root: root.to_owned(),
            layers: RefCell::default(),
            failed: RefCell::default(),
        }
    }

    pub(super) fn conversion(&self, path: &[u8]) -> Conversion {
        let mut chain = Vec::new();
        chain.extend(self.layer(b""));
        for (at, _) in path.iter().enumerate().filter(|(_, b)| **b == b'/') {
            chain.extend(self.layer(&path[..at]));
        }
        attributes::conversion(chain.iter().map(Rc::as_ref), path)
    }

    /// Stored `blob` of `path` as it is written to this working tree, when
    /// that differs.
    pub(super) fn to_worktree(&self, path: &[u8], blob: &[u8]) -> Res<Option<Vec<u8>>> {
        let converted = attributes::to_worktree(self.conversion(path), blob);
        self.check()?;
        Ok(converted)
    }

    /// The first error met while converting. A scan cannot stop in the
    /// middle, so its caller asks afterwards.
    pub(super) fn check(&self) -> Res<()> {
        match self.failed.borrow_mut().take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn fail(&self, e: Box<dyn std::error::Error>) {
        self.failed.borrow_mut().get_or_insert(e);
    }

    fn layer(&self, dir: &[u8]) -> Option<Rc<Layer>> {
        if let Some(known) = self.layers.borrow().get(dir) {
            return known.clone();
        }
        let layer = self.read(dir).filter(|l| !l.is_empty()).map(Rc::new);
        self.layers.borrow_mut().insert(dir.to_vec(), layer.clone());
        layer
    }

    fn read(&self, dir: &[u8]) -> Option<Layer> {
        let file = self.root.join(dir.to_path().ok()?).join(".gitattributes");
        // git does not follow a symlinked attributes file either
        if !file.symlink_metadata().is_ok_and(|m| m.is_file()) {
            return None;
        }
        match std::fs::read(&file) {
            Ok(content) => Some(Layer::parse(&content, dir)),
            Err(e) => {
                self.fail(e.into());
                None
            }
        }
    }
}

impl NativeRepo<'_> {
    /// The attribute files among a tree's entries, read from the store.
    pub(super) fn tree_attributes(&self, entries: &[WorkEntry]) -> Res<Vec<Layer>> {
        let mut layers = Vec::new();
        for e in entries {
            let dir = match e.path.as_slice() {
                b".gitattributes" => &b""[..],
                p => match p.strip_suffix(b"/.gitattributes") {
                    Some(dir) => dir,
                    None => continue,
                },
            };
            if e.mode != 0o100644 && e.mode != 0o100755 {
                continue;
            }
            let layer = Layer::parse(&self.blob_bytes(e.oid)?, dir);
            if !layer.is_empty() {
                layers.push(layer);
            }
        }
        Ok(layers)
    }

    /// Working-tree `content` of `path` as it is stored, when that differs.
    /// `index` is where an `auto` path's staged content is looked up; a
    /// failure to read it is left with `attrs`.
    pub(super) fn eol_to_store(
        &self,
        attrs: &WorktreeAttributes,
        index: &Index,
        path: &[u8],
        content: &[u8],
    ) -> Option<Vec<u8>> {
        attributes::to_store(attrs.conversion(path), content, || {
            let staged = index
                .entries
                .iter()
                .find(|e| e.stage() == 0 && e.path.as_slice() == path);
            match staged.map(|e| self.blob_bytes(e.oid)) {
                Some(Ok(blob)) => attributes::stored_with_crlf(&blob),
                Some(Err(e)) => {
                    attrs.fail(e);
                    false
                }
                None => false,
            }
        })
    }
}

/// Stored `blob` of `path` as a checkout under `layers` writes it, when
/// that differs.
pub(super) fn eol_to_worktree(layers: &[Layer], path: &[u8], blob: &[u8]) -> Option<Vec<u8>> {
    attributes::to_worktree(attributes::conversion(layers, path), blob)
}
