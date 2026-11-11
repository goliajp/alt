use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    sync::Arc,
};

#[derive(Clone)]

pub struct Assets {
    files: BTreeMap<String, Asset>,
}
#[derive(Clone)]

pub struct Asset {
    pub bytes: Arc<[u8]>,
    pub content_type: &'static str,
}
impl Assets {
    /// Freeze a bounded regular-file set at startup. Later filesystem changes
    /// cannot retarget requests, and the HTTP path is never a filesystem path.
    pub fn load(root: &Path) -> io::Result<Self> {
        if !root.symlink_metadata()?.is_dir() {
            return Err(io::Error::other("workbench root is not a directory"));
        }
        let mut files = BTreeMap::new();
        let mut total = 0u64;
        fn visit(
            root: &Path,
            path: &Path,
            files: &mut BTreeMap<String, Asset>,
            total: &mut u64,
            depth: usize,
        ) -> io::Result<()> {
            if depth > 4 {
                return Err(io::Error::other("workbench asset depth"));
            }
            for entry in fs::read_dir(path)? {
                let entry = entry?;
                let kind = entry.file_type()?;
                let path = entry.path();
                if kind.is_symlink() {
                    return Err(io::Error::other("linked workbench asset"));
                }
                if kind.is_dir() {
                    visit(root, &path, files, total, depth + 1)?;
                    continue;
                }
                if !kind.is_file() {
                    return Err(io::Error::other("workbench asset is not a regular file"));
                }
                let relative = path
                    .strip_prefix(root)
                    .map_err(|_| io::Error::other("asset scope"))?
                    .to_str()
                    .ok_or_else(|| io::Error::other("asset filename"))?;
                if relative.len() > 256
                    || relative.contains('\\')
                    || relative.contains('%')
                    || !relative.is_ascii()
                    || files.len() >= 1024
                {
                    return Err(io::Error::other("workbench asset name/count"));
                }
                // Source maps are an installation artifact, not an HTTP resource.
                if relative.ends_with(".map") {
                    continue;
                }
                let content_type = match path.extension().and_then(|v| v.to_str()) {
                    Some("html") => "text/html; charset=utf-8",
                    Some("js") => "text/javascript; charset=utf-8",
                    Some("css") => "text/css; charset=utf-8",
                    Some("svg") => "image/svg+xml",
                    Some("png") => "image/png",
                    Some("woff2") => "font/woff2",
                    _ => return Err(io::Error::other("unsupported workbench asset type")),
                };
                let file = fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(alt_os::sys::O_NOFOLLOW | alt_os::sys::O_CLOEXEC)
                    .open(&path)?;
                if !file.metadata()?.is_file() {
                    return Err(io::Error::other("workbench asset changed type"));
                }
                let mut bytes = Vec::new();
                file.take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
                *total = total
                    .checked_add(bytes.len() as u64)
                    .ok_or_else(|| io::Error::other("asset bytes"))?;
                if *total > 16 * 1024 * 1024 {
                    return Err(io::Error::other("workbench asset bytes"));
                }
                let key = if relative == "index.html" {
                    "/".into()
                } else {
                    format!("/{relative}")
                };
                files.insert(
                    key,
                    Asset {
                        bytes: bytes.into(),
                        content_type,
                    },
                );
            }
            Ok(())
        }
        visit(root, root, &mut files, &mut total, 0)?;
        if !files.contains_key("/") {
            return Err(io::Error::other("workbench index missing"));
        }
        Ok(Self { files })
    }
    pub fn get(&self, path: &str) -> Option<&Asset> {
        self.files.get(path)
    }
}
