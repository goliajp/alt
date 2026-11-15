//! A filter that stays running and converts one file after another:
//! git's long-running filter process, version 2 of its protocol. Requests
//! and answers are lists of text lines and runs of content, each in
//! packets of at most 65516 bytes behind a four-digit length, a list or a
//! run ended by an empty packet. Delayed answers are not asked for.

use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

const MAX_PACKET: usize = 65516;

pub(super) struct FilterProcess {
    child: Child,
    /// taken away to end the process
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
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
            output,
            capabilities: Vec::new(),
        };
        process.send(&["git-filter-client", "version=2"])?;
        let welcome = process.list()?;
        if welcome != ["git-filter-server", "version=2"] {
            return Err(format!("unexpected answer from the filter process: {welcome:?}").into());
        }
        process.send(&["capability=clean", "capability=smudge"])?;
        for line in process.list()? {
            if let Some(capability) = line.strip_prefix("capability=") {
                process.capabilities.push(capability.to_owned());
            }
        }
        Ok(process)
    }

    fn packet(&mut self, payload: &[u8]) -> Res<()> {
        let input = self.input.as_mut().expect("the process is running");
        write!(input, "{:04x}", payload.len() + 4)?;
        Ok(input.write_all(payload)?)
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

    /// The next packet; `None` for the empty one that ends a list.
    fn read(&mut self) -> Res<Option<Vec<u8>>> {
        let mut head = [0u8; 4];
        self.output.read_exact(&mut head)?;
        let len = std::str::from_utf8(&head)
            .ok()
            .and_then(|digits| usize::from_str_radix(digits, 16).ok())
            .ok_or("the filter process sent a malformed packet")?;
        match len {
            0 => Ok(None),
            1..=3 => Err("the filter process sent a malformed packet".into()),
            _ => {
                let mut payload = vec![0; len - 4];
                self.output.read_exact(&mut payload)?;
                Ok(Some(payload))
            }
        }
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
        // the end of its input tells the process to finish
        self.input = None;
        let _ = self.child.wait();
    }
}
