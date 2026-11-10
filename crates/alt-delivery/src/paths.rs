use super::*;
use std::{
    ffi::CString,
    fs::{File, OpenOptions},
    os::unix::{
        ffi::OsStrExt,
        fs::{PermissionsExt, symlink},
    },
};

/// Probe real directory entries on the destination filesystem. Distinct names
/// must create distinct entries, including on case/normalization-insensitive APFS.
pub fn preflight_paths(
    parent: &Path,
    entries: &BTreeMap<String, FileEntry>,
    odb: &NativeOdb,
) -> Result<()> {
    alt_input::check_execution_platform(parent)?;
    probe_names(parent, entries.keys().map(String::as_str))?;
    validate_files(entries, odb)
}
/// Check execution and delivery semantics without writing a checkout or probing
/// a filesystem. Review and integration use the same rules as publication.
pub fn validate_files(entries: &BTreeMap<String, FileEntry>, odb: &NativeOdb) -> Result<()> {
    for (path, entry) in entries {
        alt_input::checked_path(path.as_bytes())?;
        if path
            .split('/')
            .any(|part| part.eq_ignore_ascii_case(".git") || part.eq_ignore_ascii_case(".alt"))
        {
            return Err("repository control path in source tree".into());
        }
        if !matches!(entry.mode, 0o100644 | 0o100755 | 0o120000) {
            return Err("unsupported checkout mode".into());
        }
        let object = odb
            .get(&ObjectId::from_hex(entry.oid.as_bytes())?)?
            .ok_or("missing file object")?;
        if object.kind != ObjectKind::Blob {
            return Err("file points to non-blob".into());
        }
        let link = entry.mode == 0o120000;
        let node = alt_input::Node {
            kind: if link {
                alt_input::EntryKind::Symlink
            } else {
                alt_input::EntryKind::File
            },
            executable: entry.mode == 0o100755,
            len: object.data.len() as u64,
            content: alt_hash::blake3::hash(&object.data).to_hex().to_string(),
            link_target: link.then(|| alt_input::hex(&object.data)),
        };
        node.validate(path)?;
        if path.rsplit('/').next() == Some(".gitattributes")
            && (object
                .data
                .windows(b"filter=".len())
                .any(|v| v == b"filter=")
                || object
                    .data
                    .windows(b"working-tree-encoding".len())
                    .any(|v| v == b"working-tree-encoding"))
        {
            return Err("source attributes require external filtering/encoding input".into());
        }
        if object
            .data
            .starts_with(b"version https://git-lfs.github.com/spec/v1\n")
        {
            return Err("LFS pointer needs explicitly authorized complete input".into());
        }
    }
    Ok(())
}
pub fn materialize(
    root: &Path,
    entries: &BTreeMap<String, FileEntry>,
    odb: &NativeOdb,
) -> Result<()> {
    preflight_paths(root, entries, odb)?;
    for (path, entry) in entries {
        let target = root.join(path);
        fs::create_dir_all(target.parent().ok_or("missing parent")?)?;
        let bytes = odb
            .get(&ObjectId::from_hex(entry.oid.as_bytes())?)?
            .ok_or("missing checkout blob")?
            .data;
        if entry.mode == 0o120000 {
            symlink(std::ffi::OsStr::from_bytes(&bytes), &target)?;
        } else {
            use std::io::Write;
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)?;
            file.write_all(&bytes)?;
            file.set_permissions(fs::Permissions::from_mode(if entry.mode == 0o100755 {
                0o755
            } else {
                0o644
            }))?;
            file.sync_all()?;
        }
    }
    verify_checkout(root, entries, odb)
}
pub fn verify_checkout(
    root: &Path,
    entries: &BTreeMap<String, FileEntry>,
    odb: &NativeOdb,
) -> Result<()> {
    let input = alt_input::Root::open(root)?;
    for (path, entry) in entries {
        use alt_input::Tree;
        let node = input.inspect(path)?;
        let bytes = odb
            .get(&ObjectId::from_hex(entry.oid.as_bytes())?)?
            .ok_or("missing checkout object")?
            .data;
        if node.content != alt_hash::blake3::hash(&bytes).to_hex().as_str()
            || node.len != bytes.len() as u64
            || node.executable != (entry.mode == 0o100755)
            || (node.kind == alt_input::EntryKind::Symlink) != (entry.mode == 0o120000)
        {
            return Err(
                format!("checkout differs bytes={}", alt_input::hex(path.as_bytes())).into(),
            );
        }
    }
    let mut queue = vec![root.to_path_buf()];
    while let Some(dir) = queue.pop() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let relative = path.strip_prefix(root)?;
            if relative == Path::new(".git") || relative == Path::new(".alt") {
                continue;
            }
            let name = alt_input::checked_path(relative.as_os_str().as_bytes())?;
            if entry.file_type()?.is_dir() {
                queue.push(path);
            } else if !entries.contains_key(name) {
                return Err("unexpected checkout file".into());
            }
        }
    }
    Ok(())
}
/// No replace: a racing destination, including an empty directory, is never overwritten.
pub fn publish_directory(staging: &Path, target: &Path) -> Result<()> {
    fn sync_tree(path: &Path) -> std::io::Result<()> {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                sync_tree(&entry.path())?;
            } else if kind.is_file() {
                File::open(entry.path())?.sync_all()?;
            }
        }
        File::open(path)?.sync_all()
    }
    sync_tree(staging)?;
    let from = CString::new(staging.as_os_str().as_bytes())?;
    let to = CString::new(target.as_os_str().as_bytes())?;
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    let result = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let result = -1;
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    File::open(target.parent().ok_or("missing target parent")?)?.sync_all()?;
    Ok(())
}

pub(super) fn probe_names<'a>(
    parent: &Path,
    paths: impl IntoIterator<Item = &'a str>,
) -> Result<()> {
    let probe = tempfile::tempdir_in(parent)?;
    let mut directories = BTreeSet::new();
    for path in paths {
        alt_input::checked_path(path.as_bytes())?;
        let mut prefix = String::new();
        let parts: Vec<_> = path.split('/').collect();
        for part in &parts[..parts.len() - 1] {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            if directories.insert(prefix.clone()) {
                fs::create_dir(probe.path().join(&prefix)).map_err(|e| {
                    format!(
                        "destination path collision/limit bytes={}: {e}",
                        alt_input::hex(prefix.as_bytes())
                    )
                })?;
            }
        }
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(probe.path().join(path))
            .map_err(|e| {
                format!(
                    "destination path collision/limit bytes={}: {e}",
                    alt_input::hex(path.as_bytes())
                )
            })?;
    }
    Ok(())
}
