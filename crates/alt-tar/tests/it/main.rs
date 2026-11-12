mod fixtures;
mod reject;
mod roundtrip;
mod system;

use alt_tar::{Archive, Builder, Kind};
use std::io::Read;

type Members = Vec<(Vec<u8>, Vec<u8>)>;

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/it/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(path).unwrap()
}

fn write(members: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
    let mut builder = Builder::new(Vec::new());
    for (path, data) in members {
        builder
            .append_file(path, 0o600, 0, data.len() as u64, &mut &data[..])
            .unwrap();
    }
    builder.finish().unwrap()
}

fn read_files(archive: &[u8]) -> alt_tar::Result<Members> {
    let mut archive = Archive::new(archive);
    let mut members = Vec::new();
    while let Some(mut entry) = archive.next_entry()? {
        if entry.kind() != Kind::File {
            continue;
        }
        let mut data = Vec::new();
        entry.read_to_end(&mut data)?;
        assert_eq!(data.len() as u64, entry.size());
        members.push((entry.path().to_vec(), data));
    }
    Ok(members)
}

/// a raw header block with a valid checksum
fn header(name: &[u8], kind: u8, size: &[u8]) -> [u8; 512] {
    let mut block = [0u8; 512];
    block[..name.len()].copy_from_slice(name);
    block[100..108].copy_from_slice(b"0000600\0");
    block[124..124 + size.len()].copy_from_slice(size);
    block[156] = kind;
    block[257..263].copy_from_slice(b"ustar\0");
    block[263..265].copy_from_slice(b"00");
    seal(&mut block);
    block
}

fn seal(block: &mut [u8; 512]) {
    block[148..156].fill(b' ');
    let sum: u32 = block.iter().map(|&b| u32::from(b)).sum();
    block[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
}

fn octal(size: usize) -> Vec<u8> {
    format!("{size:011o}\0").into_bytes()
}

/// header, data and padding of one member
fn member(name: &[u8], kind: u8, data: &[u8]) -> Vec<u8> {
    let mut bytes = header(name, kind, &octal(data.len())).to_vec();
    bytes.extend_from_slice(data);
    bytes.resize(bytes.len().next_multiple_of(512), 0);
    bytes
}
