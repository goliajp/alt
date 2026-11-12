use super::Parser;
use super::path::{is_drive_letter, is_normalized_drive_letter, starts_with_drive_letter};
use crate::host::HostKind;
use crate::{ParseError, Url, host};

/// The drive letter segment that starts the path of a file URL, if any.
fn drive(base: &Url) -> Option<&str> {
    let first = base.path().get(1..)?.split('/').next()?;
    is_normalized_drive_letter(first).then_some(first)
}

impl Parser {
    /// Parses what follows `file:`, or a relative input against a file `base`.
    pub(super) fn file<'i>(
        &mut self,
        input: &'i str,
        base: Option<&Url>,
    ) -> Result<&'i str, ParseError> {
        self.out.push_str("//");
        self.authority = true;
        self.username_end = self.out.len();
        self.host_start = self.out.len();
        let bytes = input.as_bytes();
        let slash = |byte: Option<&u8>| matches!(byte, Some(b'/' | b'\\'));
        if slash(bytes.first()) && slash(bytes.get(1)) {
            return self.file_host(&input[2..]);
        }
        if let Some(base) = base {
            self.out.push_str(base.host_str().unwrap_or(""));
            self.host = base.host;
        }
        self.host_end = self.out.len();
        self.path_start = self.out.len();
        if slash(bytes.first()) {
            let rest = &input[1..];
            if !starts_with_drive_letter(rest)
                && let Some(drive) = base.and_then(drive)
            {
                self.out.push('/');
                self.out.push_str(drive);
            }
            return Ok(self.path(rest));
        }
        let Some(base) = base else {
            return Ok(self.path(input));
        };
        self.out.push_str(base.path());
        match bytes.first() {
            None | Some(b'#') => self.copy_query(base),
            Some(b'?') => {}
            Some(_) => {
                if starts_with_drive_letter(input) {
                    self.out.truncate(self.path_start);
                } else {
                    self.shorten_path();
                }
                return Ok(self.path(input));
            }
        }
        Ok(input)
    }

    fn file_host<'i>(&mut self, input: &'i str) -> Result<&'i str, ParseError> {
        let end = input.find(['/', '\\', '?', '#']).unwrap_or(input.len());
        let (name, rest) = input.split_at(end);
        if is_drive_letter(name) {
            self.host_end = self.out.len();
            self.path_start = self.out.len();
            return Ok(self.path(input));
        }
        if !name.is_empty() {
            self.host = host::parse(&mut self.out, name, true)?;
            if &self.out[self.host_start..] == "localhost" {
                self.out.truncate(self.host_start);
                self.host = HostKind::None;
            }
        }
        self.host_end = self.out.len();
        Ok(self.path_start(rest))
    }
}
