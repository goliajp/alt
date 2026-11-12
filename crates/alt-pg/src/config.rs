//! Connection parameters in the `key=value` form libpq documents.

use crate::{Error, Result};
use std::{net::IpAddr, path::PathBuf, str::FromStr, time::Duration};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Host {
    Tcp(String),
    /// the directory holding the server's socket
    Unix(PathBuf),
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct Config {
    pub(crate) host: Option<Host>,
    pub(crate) hostaddr: Option<IpAddr>,
    pub(crate) port: Option<u16>,
    pub(crate) user: Option<String>,
    pub(crate) password: Option<String>,
    pub(crate) dbname: Option<String>,
    pub(crate) application_name: Option<String>,
    pub(crate) options: Option<String>,
    pub(crate) connect_timeout: Option<Duration>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("host", &self.host)
            .field("hostaddr", &self.hostaddr)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("password", &self.password.as_ref().map(|_| "_"))
            .field("dbname", &self.dbname)
            .field("application_name", &self.application_name)
            .field("options", &self.options)
            .field("connect_timeout", &self.connect_timeout)
            .finish()
    }
}

impl Config {
    pub fn host(&self) -> Option<&Host> {
        self.host.as_ref()
    }
    pub fn hostaddr(&self) -> Option<IpAddr> {
        self.hostaddr
    }
    pub fn port(&self) -> Option<u16> {
        self.port
    }
    pub fn user(&self) -> Option<&str> {
        self.user.as_deref()
    }
    pub fn password(&self) -> Option<&str> {
        self.password.as_deref()
    }
    pub fn dbname(&self) -> Option<&str> {
        self.dbname.as_deref()
    }
    pub fn set_dbname(&mut self, dbname: &str) -> &mut Self {
        self.dbname = Some(dbname.into());
        self
    }
    pub fn set_user(&mut self, user: &str) -> &mut Self {
        self.user = Some(user.into());
        self
    }
    pub fn set_password(&mut self, password: &str) -> &mut Self {
        self.password = Some(password.into());
        self
    }
    pub fn set_application_name(&mut self, name: &str) -> &mut Self {
        self.application_name = Some(name.into());
        self
    }
    /// bounds establishing a TCP connection; a socket connection is not bounded
    pub fn set_connect_timeout(&mut self, timeout: Duration) -> &mut Self {
        self.connect_timeout = Some(timeout);
        self
    }
    pub fn connect(&self) -> Result<crate::Client> {
        crate::Client::connect_with(self)
    }

    fn set(&mut self, key: &str, value: String) -> Result<()> {
        match key {
            "host" => {
                if value.contains(',') {
                    return Err(Error::Config("more than one host"));
                }
                self.host = Some(if value.starts_with('/') {
                    Host::Unix(value.into())
                } else {
                    Host::Tcp(value)
                });
            }
            "hostaddr" => {
                self.hostaddr = Some(value.parse().map_err(|_| Error::Config("hostaddr"))?);
            }
            "port" => self.port = Some(value.parse().map_err(|_| Error::Config("port"))?),
            "user" => self.user = Some(value),
            "password" => self.password = Some(value),
            "dbname" => self.dbname = Some(value),
            "application_name" => self.application_name = Some(value),
            "options" => self.options = Some(value),
            "connect_timeout" => {
                let seconds: u64 = value
                    .parse()
                    .map_err(|_| Error::Config("connect_timeout"))?;
                self.connect_timeout = (seconds > 0).then(|| Duration::from_secs(seconds));
            }
            "sslmode" => {
                if !matches!(value.as_str(), "disable" | "allow" | "prefer") {
                    return Err(Error::Config("TLS is not supported"));
                }
            }
            "client_encoding" => {
                if !value.eq_ignore_ascii_case("utf8") && !value.eq_ignore_ascii_case("utf-8") {
                    return Err(Error::Config("client_encoding must be UTF8"));
                }
            }
            _ => return Err(Error::Config("unknown connection parameter")),
        }
        Ok(())
    }
}

impl FromStr for Config {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self> {
        let mut config = Self::default();
        let mut rest = text.trim_start();
        while !rest.is_empty() {
            let (key, after) = rest
                .split_once('=')
                .ok_or(Error::Config("parameter without a value"))?;
            let key = key.trim_end();
            if key.is_empty() || key.contains(char::is_whitespace) {
                return Err(Error::Config("parameter name"));
            }
            let (value, after) = value(after.trim_start())?;
            config.set(key, value)?;
            rest = after.trim_start();
        }
        Ok(config)
    }
}

/// one value and the text after it; a quoted value may hold spaces
fn value(text: &str) -> Result<(String, &str)> {
    let mut out = String::new();
    let mut chars = text.char_indices();
    let quoted = text.starts_with('\'');
    if quoted {
        chars.next();
    }
    while let Some((at, c)) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some((_, escaped)) => out.push(escaped),
                None => return Err(Error::Config("unfinished escape")),
            },
            '\'' if quoted => return Ok((out, &text[at + 1..])),
            c if c.is_whitespace() && !quoted => return Ok((out, &text[at..])),
            c => out.push(c),
        }
    }
    if quoted {
        return Err(Error::Config("unterminated quoted value"));
    }
    Ok((out, ""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_socket_and_tcp_forms() {
        let config: Config = "host=/run/pg user=alt_test dbname=postgres connect_timeout=5"
            .parse()
            .unwrap();
        assert_eq!(config.host(), Some(&Host::Unix("/run/pg".into())));
        assert_eq!(config.user(), Some("alt_test"));
        assert_eq!(config.dbname(), Some("postgres"));
        assert_eq!(config.connect_timeout, Some(Duration::from_secs(5)));
        let config: Config = " host = 127.0.0.1 port=5433 user=u password='a b\\'c\\\\' "
            .parse()
            .unwrap();
        assert_eq!(config.host(), Some(&Host::Tcp("127.0.0.1".into())));
        assert_eq!(config.port(), Some(5433));
        assert_eq!(config.password(), Some("a b'c\\"));
        assert_eq!("".parse::<Config>().unwrap(), Config::default());
    }

    #[test]
    fn rejects_what_it_cannot_honour() {
        for text in [
            "host",
            "=x",
            "host=a,b",
            "port=70000",
            "password='open",
            "sslmode=require",
            "keepalives=1",
            "hostaddr=localhost",
            "user=a\\",
        ] {
            assert!(text.parse::<Config>().is_err(), "{text}");
        }
    }

    #[test]
    fn debug_hides_the_password() {
        let config: Config = "password=sentinel".parse().unwrap();
        assert!(!format!("{config:?}").contains("sentinel"));
    }
}
