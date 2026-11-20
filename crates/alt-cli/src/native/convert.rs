//! Conversion of file content between the object store and the working
//! tree, as the git attributes of a path and the repository's
//! configuration ask: line endings, the `$Id$` keyword, and filter
//! commands. LFS, which alt handles itself, is not a filter here.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use alt_worktree::attributes::{self, AutoCrlf, CoreEol, EolConfig, Layer, PathAttributes};
use alt_worktree::ident;

use super::*;
use crate::git_settings::read_if_file;

mod filter_process;
mod stored;

use filter_process::{FilterProcess, Malformed};

/// The commands of one `filter.<name>` section.
#[derive(Default)]
struct Driver {
    clean: Option<Vec<u8>>,
    smudge: Option<Vec<u8>>,
    /// a command that stays running and does both, in place of the two
    process: Option<Vec<u8>>,
    /// a failing command is an error instead of leaving the content as is
    required: bool,
}

/// The digest of a file's content and what the clean filter made of it;
/// `None` when the filter left it as it was.
type Cleaned = ([u8; 32], Option<Vec<u8>>);

/// Where the `.gitattributes` files are read from.
enum Tree {
    /// the working tree: each file is read the first time a path beneath
    /// its directory is asked about
    Disk(RefCell<HashMap<Vec<u8>, Option<Rc<Layer>>>>),
    /// the tree being checked out
    Stored(Vec<Layer>),
}

/// Converts content for one working tree.
pub(super) struct Converter {
    root: PathBuf,
    tree: Tree,
    /// the user's attributes file and the repository's own
    fixed: Vec<Layer>,
    config: EolConfig,
    drivers: HashMap<Vec<u8>, Driver>,
    /// the filter processes running, by driver
    processes: RefCell<HashMap<Vec<u8>, FilterProcess>>,
    /// what the clean filter last made of each path, with the digest of
    /// the content it was given: a file is scanned and then stored, and
    /// both must see the same result
    cleaned: RefCell<HashMap<Vec<u8>, Cleaned>>,
    algo: HashAlgo,
    failed: RefCell<Option<Box<dyn std::error::Error>>>,
}

impl NativeRepo<'_> {
    /// A converter that reads `.gitattributes` from the working tree.
    pub(super) fn converter(&self) -> Res<Converter> {
        self.converter_over(Tree::Disk(RefCell::default()))
    }

    /// A converter for checking out `entries`, whose `.gitattributes`
    /// files are read from the store.
    pub(super) fn converter_for(&self, entries: &[WorkEntry]) -> Res<Converter> {
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
            layers.push(Layer::parse(&self.blob_bytes(e.oid)?, dir));
        }
        self.converter_over(Tree::Stored(layers))
    }

    /// A converter under the given `.gitattributes` files, each with the
    /// directory it lies in.
    pub(super) fn converter_under(&self, files: &[(&[u8], &[u8])]) -> Res<Converter> {
        let layers = files
            .iter()
            .map(|(dir, content)| Layer::parse(content, dir));
        self.converter_over(Tree::Stored(layers.collect()))
    }

    fn converter_over(&self, tree: Tree) -> Res<Converter> {
        let settings = self.settings()?;
        let text = |key: &str| settings.get_str("core", None, key).map(|v| v.to_vec());
        let autocrlf = match text("autocrlf").as_deref() {
            Some(b"input") => AutoCrlf::Input,
            _ => match settings.get_bool("core", None, "autocrlf").transpose()? {
                Some(true) => AutoCrlf::True,
                _ => AutoCrlf::False,
            },
        };
        let eol = match text("eol").as_deref() {
            Some(b"crlf") => CoreEol::Crlf,
            Some(b"lf") => CoreEol::Lf,
            _ => CoreEol::Native,
        };
        let mut drivers: HashMap<Vec<u8>, Driver> = HashMap::new();
        for e in settings.entries.iter().filter(|e| e.section == "filter") {
            let (Some(name), Some(value)) = (&e.subsection, &e.value) else {
                continue;
            };
            let driver = drivers.entry(name.to_vec()).or_default();
            match e.key.as_str() {
                "clean" => driver.clean = Some(value.to_vec()),
                "smudge" => driver.smudge = Some(value.to_vec()),
                "process" => driver.process = Some(value.to_vec()),
                "required" => {
                    driver.required = settings
                        .get_bool("filter", Some(name.as_slice()), "required")
                        .transpose()?
                        .unwrap_or(false);
                }
                _ => {}
            }
        }
        let user = self.user_attributes(&settings)?;
        let info = self.info_attributes();
        let fixed = crate::git_settings::outer_attributes(user.as_deref(), &info)?;
        Ok(Converter {
            root: self.root.clone(),
            tree,
            fixed,
            config: EolConfig { autocrlf, eol },
            drivers,
            processes: RefCell::default(),
            cleaned: RefCell::default(),
            algo: self.store.algo,
            failed: RefCell::default(),
        })
    }
}

impl Converter {
    pub(super) fn attributes(&self, path: &[u8]) -> PathAttributes {
        let mut chain: Vec<Rc<Layer>> = Vec::new();
        let stored: &[Layer] = match &self.tree {
            Tree::Stored(layers) => layers,
            Tree::Disk(_) => {
                chain.extend(self.layer(b""));
                for (at, _) in path.iter().enumerate().filter(|(_, b)| **b == b'/') {
                    chain.extend(self.layer(&path[..at]));
                }
                &[]
            }
        };
        let layers = self
            .fixed
            .iter()
            .chain(stored)
            .chain(chain.iter().map(Rc::as_ref));
        attributes::lookup(layers, path, &self.config)
    }

    /// The first error met while converting. A scan cannot stop in the
    /// middle, so its caller asks afterwards.
    pub(super) fn check(&self) -> Res<()> {
        match self.failed.borrow_mut().take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// The commit the repository in the submodule directory `dir` has
    /// checked out; `None` when the directory holds no repository. A
    /// repository that cannot be read is a failure kept for
    /// [`check`](Self::check).
    pub(super) fn submodule_head(&self, dir: &Path) -> Option<ObjectId> {
        let holds = |name: &str| dir.join(name).symlink_metadata().is_ok();
        if !holds(".git") && !holds(".alt") {
            return None;
        }
        let head = alt_repo::Repository::discover(dir).and_then(|repo| repo.resolve_ref("HEAD"));
        match head {
            Ok(oid) => oid,
            Err(e) => {
                self.fail(e.into());
                None
            }
        }
    }

    fn fail(&self, e: Box<dyn std::error::Error>) {
        self.failed.borrow_mut().get_or_insert(e);
    }

    fn layer(&self, dir: &[u8]) -> Option<Rc<Layer>> {
        let Tree::Disk(cache) = &self.tree else {
            return None;
        };
        if let Some(known) = cache.borrow().get(dir) {
            return known.clone();
        }
        let file = dir
            .to_path()
            .ok()
            .map(|d| self.root.join(d).join(".gitattributes"));
        let layer = match file.map(read_if_file) {
            Some(Ok(content)) => content.map(|c| Rc::new(Layer::parse(&c, dir))),
            Some(Err(e)) => {
                self.fail(e);
                None
            }
            None => None,
        };
        let layer = layer.filter(|l| !l.is_empty());
        cache.borrow_mut().insert(dir.to_vec(), layer.clone());
        layer
    }

    /// Working-tree `content` as it is stored: through the clean filter,
    /// then line endings, then the `$Id$` keyword. `None` when it is
    /// stored as it is. A failure is kept for [`check`](Self::check).
    fn to_store(
        &self,
        path: &[u8],
        content: &[u8],
        stored_crlf: &dyn Fn() -> bool,
    ) -> Option<Vec<u8>> {
        let attrs = self.attributes(path);
        let mut out: Option<Vec<u8>> = None;
        if let Some((name, driver)) = self.driver(&attrs) {
            let digest = *alt_hash::blake3::hash(content).as_bytes();
            let known = self.cleaned.borrow().get(path).cloned();
            out = match known.filter(|(of, _)| *of == digest) {
                Some((_, cleaned)) => cleaned,
                None => {
                    let cleaned = match self.filter(name, driver, "clean", path, content) {
                        Ok(cleaned) => cleaned,
                        Err(e) if driver.required || e.is::<Malformed>() => {
                            self.fail(e);
                            return None;
                        }
                        Err(_) => None,
                    };
                    let kept = (digest, cleaned.clone());
                    self.cleaned.borrow_mut().insert(path.to_vec(), kept);
                    cleaned
                }
            };
        }
        let current = out.as_deref().unwrap_or(content);
        if let Some(normal) = attributes::to_store(attrs.conversion, current, stored_crlf) {
            out = Some(normal);
        }
        if attrs.ident
            && let Some(collapsed) = ident::to_store(out.as_deref().unwrap_or(content))
        {
            out = Some(collapsed);
        }
        out
    }

    /// Stored `blob` of `path` as it is written to the working tree: the
    /// `$Id$` keyword, then line endings, then the smudge filter. `None`
    /// when it is written as it is.
    pub(super) fn to_worktree(&self, path: &[u8], blob: &[u8]) -> Res<Option<Vec<u8>>> {
        let attrs = self.attributes(path);
        self.check()?;
        let mut out: Option<Vec<u8>> = None;
        if attrs.ident && blob.windows(3).any(|w| w == b"$Id") {
            let id = ObjectId::hash_object(self.algo, ObjectKind::Blob, blob);
            out = ident::to_worktree(blob, &id.to_string());
        }
        if let Some(ended) =
            attributes::to_worktree(attrs.conversion, out.as_deref().unwrap_or(blob))
        {
            out = Some(ended);
        }
        if let Some((name, driver)) = self.driver(&attrs) {
            match self.filter(name, driver, "smudge", path, out.as_deref().unwrap_or(blob)) {
                Ok(Some(smudged)) => out = Some(smudged),
                Ok(None) => {}
                Err(e) if driver.required || e.is::<Malformed>() => return Err(e),
                Err(_) => {}
            }
        }
        Ok(out)
    }

    fn driver<'a>(&'a self, attrs: &'a PathAttributes) -> Option<(&'a [u8], &'a Driver)> {
        let name = attrs.filter.as_deref()?;
        // alt keeps LFS content itself
        let driver = (name != b"lfs").then(|| self.drivers.get(name)).flatten()?;
        Some((name, driver))
    }

    /// `content` through the `clean` or `smudge` side of a driver: its
    /// process when it has one, else the command of that side. `None`
    /// when the driver has no command for the side.
    fn filter(
        &self,
        name: &[u8],
        driver: &Driver,
        side: &str,
        path: &[u8],
        content: &[u8],
    ) -> Res<Option<Vec<u8>>> {
        let Some(command) = &driver.process else {
            let command = match side {
                "clean" => &driver.clean,
                _ => &driver.smudge,
            };
            return command
                .as_ref()
                .map(|command| self.run(command, path, content))
                .transpose();
        };
        let processes = &mut self.processes.borrow_mut();
        filter_process::through(processes, name, command, &self.root, side, path, content)
    }

    /// Runs a filter command through the shell, as git does: `%f` in it
    /// stands for the quoted path, the content goes in on standard input
    /// and the result is what it prints.
    fn run(&self, command: &[u8], path: &[u8], input: &[u8]) -> Res<Vec<u8>> {
        use std::io::Read;
        use std::os::unix::ffi::OsStrExt;
        use std::process::{Command, Stdio};
        let mut quoted = b"'".to_vec();
        for &b in path {
            match b {
                b'\'' => quoted.extend_from_slice(b"'\\''"),
                _ => quoted.push(b),
            }
        }
        quoted.push(b'\'');
        let mut line = Vec::new();
        let mut rest = command;
        while let Some(at) = rest.windows(2).position(|w| w == b"%f") {
            line.extend_from_slice(&rest[..at]);
            line.extend_from_slice(&quoted);
            rest = &rest[at + 2..];
        }
        line.extend_from_slice(rest);
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(std::ffi::OsStr::from_bytes(&line))
            .current_dir(&self.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let mut stdin = child.stdin.take().expect("piped");
        let mut stdout = child.stdout.take().expect("piped");
        // written from another thread: a filter may print before it has
        // read everything
        let (written, read) = std::thread::scope(|scope| {
            let writer = scope.spawn(move || stdin.write_all(input));
            let mut out = Vec::new();
            let read = stdout.read_to_end(&mut out).map(|_| out);
            (writer.join().expect("writer thread"), read)
        });
        let status = child.wait()?;
        let name = String::from_utf8_lossy(command);
        if !status.success() {
            return Err(format!("filter command `{name}` failed: {status}").into());
        }
        // a filter that does not read all of its input still succeeded
        match written {
            Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => return Err(e.into()),
            _ => {}
        }
        Ok(read?)
    }
}
