//! Times archive writing and reading through pipes, the way a backup uses them.
//!
//!   tar_speed gen <dir> <mib>          create the input tree
//!   tar_speed write <dir> <archive>    archive the tree through `cat`
//!   tar_speed read <archive> <dir>     extract an archive read through `cat`
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

const MIB: u64 = 1024 * 1024;

fn fill(buffer: &mut [u8], state: &mut u64) {
    for chunk in buffer.chunks_mut(8) {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        chunk.copy_from_slice(&state.to_le_bytes()[..chunk.len()]);
    }
}

fn create(path: &Path, size: u64, state: &mut u64) -> io::Result<()> {
    fs::create_dir_all(path.parent().unwrap())?;
    let mut file = File::create(path)?;
    let mut buffer = vec![0; 1 << 20];
    let mut left = size;
    while left > 0 {
        let step = left.min(buffer.len() as u64) as usize;
        fill(&mut buffer[..step], state);
        file.write_all(&buffer[..step])?;
        left -= step as u64;
    }
    Ok(())
}

// half the bytes in one file, a quarter in 1 MiB files, a quarter in 16 KiB files
fn generate(root: &Path, mib: u64) -> io::Result<()> {
    let mut state = 0x9e37_79b9_7f4a_7c15;
    create(&root.join("database.dump"), mib / 2 * MIB, &mut state)?;
    let repository = "r".repeat(40);
    let sizes = std::iter::repeat_n(MIB, (mib / 4) as usize)
        .chain(std::iter::repeat_n(16 * 1024, (mib / 4 * 64) as usize));
    for (index, size) in sizes.enumerate() {
        let name = format!("bytes/{repository}/{index:064x}");
        create(&root.join(name), size, &mut state)?;
    }
    Ok(())
}

fn collect(root: &Path, directory: &Path, names: &mut Vec<String>) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(root, &path, names)?;
        } else {
            let relative = path.strip_prefix(root).unwrap();
            names.push(relative.to_str().unwrap().to_owned());
        }
    }
    Ok(())
}

fn write(root: &Path, archive: &Path) -> io::Result<u64> {
    let mut names = Vec::new();
    collect(root, root, &mut names)?;
    names.sort();
    let mut child = Command::new("cat")
        .stdin(Stdio::piped())
        .stdout(File::create(archive)?)
        .spawn()?;
    let mut builder = alt_tar::Builder::new(child.stdin.take().unwrap());
    let mut total = 0;
    for name in &names {
        let mut file = File::open(root.join(name))?;
        let size = file.metadata()?.len();
        builder
            .append_file(name.as_bytes(), 0o600, 0, size, &mut file)
            .map_err(io::Error::other)?;
        total += size;
    }
    drop(builder.finish()?);
    assert!(child.wait()?.success());
    Ok(total)
}

fn read(archive: &Path, root: &Path) -> io::Result<u64> {
    let mut child = Command::new("cat")
        .arg(archive)
        .stdout(Stdio::piped())
        .spawn()?;
    let mut reader = alt_tar::Archive::new(child.stdout.take().unwrap());
    let mut total = 0;
    while let Some(mut entry) = reader.next_entry().map_err(io::Error::other)? {
        let name = String::from_utf8(entry.path().to_vec()).map_err(io::Error::other)?;
        assert_eq!(entry.kind(), alt_tar::Kind::File);
        let destination = root.join(&name);
        fs::create_dir_all(destination.parent().unwrap())?;
        let mut file = File::create_new(destination)?;
        total += entry.copy_to(&mut file)?;
    }
    io::copy(&mut reader.into_inner(), &mut io::sink())?;
    assert!(child.wait()?.success());
    Ok(total)
}

fn main() -> io::Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let [mode, first, second] = arguments.as_slice() else {
        panic!("usage: tar_speed <gen|write|read> <path> <path>");
    };
    let (first, second) = (PathBuf::from(first), second.as_str());
    if mode == "gen" {
        return generate(&first, second.parse().unwrap());
    }
    let start = Instant::now();
    let bytes = match mode.as_str() {
        "write" => write(&first, Path::new(second))?,
        "read" => read(&first, Path::new(second))?,
        _ => panic!("unknown mode {mode}"),
    };
    let seconds = start.elapsed().as_secs_f64();
    let rate = bytes as f64 / MIB as f64 / seconds;
    println!("alt-tar {mode} {bytes} bytes {seconds:.4} s {rate:.0} MiB/s");
    Ok(())
}
