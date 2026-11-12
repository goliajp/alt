use crate::{Error, Result};

#[derive(Default)]
pub(crate) struct Extensions {
    pub path: Option<Vec<u8>>,
    pub size: Option<u64>,
}

fn decimal(text: &[u8]) -> Result<u64> {
    if text.is_empty() {
        return Err(Error::Number);
    }
    text.iter().try_fold(0u64, |value, &digit| {
        if !digit.is_ascii_digit() {
            return Err(Error::Number);
        }
        value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u64::from(digit - b'0')))
            .ok_or(Error::Number)
    })
}

/// each record is `<length> <key>=<value>\n`, the length covering the whole record
pub(crate) fn parse(mut data: &[u8]) -> Result<Extensions> {
    let mut extensions = Extensions::default();
    while !data.is_empty() {
        let space = data
            .iter()
            .position(|&b| b == b' ')
            .ok_or(Error::Extension)?;
        let length = usize::try_from(decimal(&data[..space])?).map_err(|_| Error::Extension)?;
        if length < space + 3 || length > data.len() || data[length - 1] != b'\n' {
            return Err(Error::Extension);
        }
        let record = &data[space + 1..length - 1];
        let equals = record
            .iter()
            .position(|&b| b == b'=')
            .ok_or(Error::Extension)?;
        let (key, value) = (&record[..equals], &record[equals + 1..]);
        match key {
            b"path" => extensions.path = Some(value.to_vec()),
            b"size" => extensions.size = Some(decimal(value)?),
            _ if key.starts_with(b"GNU.sparse.") => return Err(Error::Unsupported),
            _ => {}
        }
        data = &data[length..];
    }
    Ok(extensions)
}
