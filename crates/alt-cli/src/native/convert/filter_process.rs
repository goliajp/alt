//! A filter that stays running and converts one file after another:
//! git's long-running filter process, version 2 of its protocol. Requests
//! and answers are lists of text lines and runs of content, each in
//! packets of at most 65516 bytes behind a four-digit length, a list or a
//! run ended by an empty packet. Delayed answers are not asked for.

use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

const MAX_PACKET: usize = 65516;

/// A packet no reading of the protocol allows. Nothing behind it in the
/// stream can be trusted, and as in git the command ends there.
#[derive(Debug)]
pub(super) struct Malformed(String);

impl std::fmt::Display for Malformed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "protocol error: bad line length {}", self.0)
    }
}

impl std::error::Error for Malformed {}

/// Writes one packet of `payload`.
fn write_packet(to: &mut impl Write, payload: &[u8]) -> Res<()> {
    if payload.len() > MAX_PACKET {
        return Err("packet write failed - data exceeds max packet size".into());
    }
    write!(to, "{:04x}", payload.len() + 4)?;
    Ok(to.write_all(payload)?)
}

/// The next packet; `None` for the empty one that ends a list.
fn read_packet(from: &mut impl Read) -> Res<Option<Vec<u8>>> {
    let mut head = [0u8; 4];
    from.read_exact(&mut head)?;
    let shown = || String::from_utf8_lossy(&head).into_owned();
    if !head.iter().all(u8::is_ascii_hexdigit) {
        return Err(Malformed(format!("character: {}", shown())).into());
    }
    let len = usize::from_str_radix(&shown(), 16).expect("four hexadecimal digits");
    match len {
        0 => Ok(None),
        1..=3 => Err(Malformed(len.to_string()).into()),
        // git names what would lie behind the length
        _ if len - 4 > MAX_PACKET => Err(Malformed((len - 4).to_string()).into()),
        _ => {
            let mut payload = vec![0; len - 4];
            from.read_exact(&mut payload)?;
            Ok(Some(payload))
        }
    }
}

pub(super) struct FilterProcess {
    child: Child,
    /// taken away to end the process
    input: Option<ChildStdin>,
    /// taken away so that a process still writing is not waited for
    output: Option<BufReader<ChildStdout>>,
    /// the conversation is out of step
    broken: bool,
    /// what the process said it can do, less what it has given up on
    capabilities: Vec<String>,
}

impl FilterProcess {
    /// Starts `command` through the shell in `root` and agrees with it
    /// on the protocol and on what it can do.
    pub(super) fn start(command: &[u8], root: &Path) -> Res<Self> {
        use std::os::unix::ffi::OsStrExt;
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(std::ffi::OsStr::from_bytes(command))
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let input = child.stdin.take().expect("piped");
        let output = BufReader::new(child.stdout.take().expect("piped"));
        let mut process = FilterProcess {
            child,
            input: Some(input),
            output: Some(output),
            broken: false,
            capabilities: Vec::new(),
        };
        let agreed = process.agree();
        process.broken = agreed.is_err();
        agreed.map(|()| process)
    }

    fn agree(&mut self) -> Res<()> {
        self.send(&["git-filter-client", "version=2"])?;
        let welcome = self.list()?;
        if welcome != ["git-filter-server", "version=2"] {
            return Err(format!("unexpected answer from the filter process: {welcome:?}").into());
        }
        self.send(&["capability=clean", "capability=smudge"])?;
        for line in self.list()? {
            if let Some(capability) = line.strip_prefix("capability=") {
                self.capabilities.push(capability.to_owned());
            }
        }
        Ok(())
    }

    fn packet(&mut self, payload: &[u8]) -> Res<()> {
        write_packet(
            self.input.as_mut().expect("the process is running"),
            payload,
        )
    }

    fn flush(&mut self) -> Res<()> {
        let input = self.input.as_mut().expect("the process is running");
        input.write_all(b"0000")?;
        Ok(input.flush()?)
    }

    /// Sends a list of text lines.
    fn send(&mut self, lines: &[&str]) -> Res<()> {
        for line in lines {
            self.packet(format!("{line}\n").as_bytes())?;
        }
        self.flush()
    }

    fn read(&mut self) -> Res<Option<Vec<u8>>> {
        read_packet(self.output.as_mut().expect("the process is running"))
    }

    /// A list of text lines, without their line ends.
    fn list(&mut self) -> Res<Vec<String>> {
        let mut lines = Vec::new();
        while let Some(packet) = self.read()? {
            let line = packet.strip_suffix(b"\n").unwrap_or(&packet);
            lines.push(String::from_utf8_lossy(line).into_owned());
        }
        Ok(lines)
    }

    /// The `status` a list carries, if any.
    fn status(&mut self) -> Res<Option<String>> {
        let lines = self.list()?;
        let status = lines.iter().rev().find_map(|l| l.strip_prefix("status="));
        Ok(status.map(str::to_owned))
    }

    /// `content` of `path` through the `clean` or `smudge` side of the
    /// process. `None` when the process leaves the file as it is: it
    /// cannot do this, or it answered that it failed on this file.
    pub(super) fn apply(
        &mut self,
        side: &str,
        path: &[u8],
        content: &[u8],
    ) -> Res<Option<Vec<u8>>> {
        if !self.capabilities.iter().any(|c| c == side) {
            return Ok(None);
        }
        let answer = self.ask(side, path, content);
        self.broken = answer.is_err();
        answer
    }

    fn ask(&mut self, side: &str, path: &[u8], content: &[u8]) -> Res<Option<Vec<u8>>> {
        self.packet(format!("command={side}\n").as_bytes())?;
        let mut name = b"pathname=".to_vec();
        name.extend_from_slice(path);
        name.push(b'\n');
        self.packet(&name)?;
        self.flush()?;
        for chunk in content.chunks(MAX_PACKET) {
            self.packet(chunk)?;
        }
        self.flush()?;

        let mut status = self.status()?;
        let mut out = Vec::new();
        if status.as_deref() == Some("success") {
            while let Some(packet) = self.read()? {
                out.extend_from_slice(&packet);
            }
            // the process may still take its success back
            status = self.status()?.or(status);
        }
        match status.as_deref() {
            Some("success") => Ok(Some(out)),
            Some("error") => Ok(None),
            // it will not do this again for any file
            Some("abort") => {
                self.capabilities.retain(|c| c != side);
                Ok(None)
            }
            other => Err(format!("unexpected status from the filter process: {other:?}").into()),
        }
    }
}

impl Drop for FilterProcess {
    fn drop(&mut self) {
        // the end of its input tells the process to finish, and one
        // still writing finds nobody reading
        self.input = None;
        self.output = None;
        if self.broken {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

/// `content` through the `side` of the process of driver `name`, which
/// is started when it is not running. A file the process leaves alone
/// counts as a failure of the filter, as in git.
pub(super) fn through(
    processes: &mut HashMap<Vec<u8>, FilterProcess>,
    name: &[u8],
    command: &[u8],
    root: &Path,
    side: &str,
    path: &[u8],
    content: &[u8],
) -> Res<Option<Vec<u8>>> {
    let shown = String::from_utf8_lossy(name);
    let failed = || format!("{side} filter '{shown}' failed");
    if !processes.contains_key(name) {
        // as in git, one that does not start is tried again for the
        // next file
        let process = match FilterProcess::start(command, root) {
            Ok(process) => process,
            Err(e) if e.is::<Malformed>() => return Err(e),
            Err(_) => {
                let command = String::from_utf8_lossy(command);
                eprintln!("error: initialization for subprocess '{command}' failed");
                return Err(failed().into());
            }
        };
        processes.insert(name.to_vec(), process);
    }
    let process = processes.get_mut(name).expect("started above");
    match process.apply(side, path, content) {
        Ok(Some(filtered)) => Ok(Some(filtered)),
        Ok(None) => Err(failed().into()),
        Err(e) => {
            // the conversation is out of step: the next file gets a new
            // process
            processes.remove(name);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(mut stream: &[u8]) -> Result<Option<Vec<u8>>, String> {
        read_packet(&mut stream).map_err(|e| e.to_string())
    }

    #[test]
    fn a_packet_is_read_only_behind_four_hexadecimal_digits_of_an_allowed_length() {
        assert_eq!(read(b"0000"), Ok(None));
        assert_eq!(read(b"0006ab"), Ok(Some(b"ab".to_vec())));
        assert_eq!(read(b"0004"), Ok(Some(Vec::new())));
        let bad = |head: &str, said: &str| {
            let mut stream = head.as_bytes().to_vec();
            stream.resize(70_000, b'x');
            let refused = read(&stream).unwrap_err();
            assert_eq!(refused, format!("protocol error: bad line length {said}"));
        };
        bad("+00a", "character: +00a");
        bad(" 00a", "character:  00a");
        bad("0003", "3");
        bad("0001", "1");
        bad("fff1", "65517");
        bad("ffff", "65531");
        let mut largest = b"fff0".to_vec();
        largest.resize(4 + MAX_PACKET, b'x');
        assert_eq!(
            read(&largest).map(|p| p.map(|p| p.len())),
            Ok(Some(MAX_PACKET))
        );
        // a stream that ends inside a packet is no packet either
        assert!(read(b"0008ab").is_err());
    }

    #[test]
    fn a_packet_is_written_whole_or_not_at_all() {
        let mut stream = Vec::new();
        write_packet(&mut stream, b"pathname=a\n").unwrap();
        assert_eq!(stream, b"000fpathname=a\n");
        let mut stream = Vec::new();
        write_packet(&mut stream, &vec![b'x'; MAX_PACKET]).unwrap();
        assert_eq!(&stream[..4], b"fff0");
        let mut stream = Vec::new();
        let refused = write_packet(&mut stream, &vec![b'x'; MAX_PACKET + 1]);
        assert!(
            refused
                .unwrap_err()
                .to_string()
                .contains("exceeds max packet size")
        );
        assert!(stream.is_empty());
    }
}
