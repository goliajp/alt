//! The tools of PostgreSQL a backup is made and restored with, as child processes.
use super::*;

pub(super) struct Process(pub(super) Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
pub(super) fn checked(process: &mut Process) -> Result<()> {
    if !process.0.wait().map_err(|_| Error::File)?.success() {
        return Err(Error::Config(
            "backup tool failed; no completed backup or restoration",
        ));
    }
    Ok(())
}
pub(super) fn postgres_command(config: &Config, program: &str) -> Result<Command> {
    let db = config.database()?;
    let mut command = Command::new(program);
    // No DSN/password in argv or diagnostic output. Discard ambient PG service
    // configuration so the operator cannot accidentally dump another database.
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default());
    let host = match db.host() {
        Some(alt_pg::Host::Tcp(host)) => host.clone(),
        Some(alt_pg::Host::Unix(path)) => path
            .to_str()
            .ok_or(Error::Config("database socket encoding"))?
            .into(),
        None => return Err(Error::Config("backup requires one explicit database host")),
    };
    if db.hostaddr().is_some() {
        return Err(Error::Config("backup requires one database endpoint"));
    }
    command
        .env("PGHOST", host)
        .env("PGPORT", db.port().unwrap_or(5432).to_string())
        .env(
            "PGDATABASE",
            db.dbname()
                .ok_or(Error::Config("explicit database name required"))?,
        )
        .env(
            "PGUSER",
            db.user()
                .ok_or(Error::Config("explicit database user required"))?,
        )
        .env(
            "PGCONNECT_TIMEOUT",
            (config.http.database_connect_timeout_ms / 1000)
                .max(1)
                .to_string(),
        )
        .env("PGSSLMODE", "disable")
        .arg("--no-password")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(password) = db.password() {
        command.env("PGPASSWORD", password);
    }
    Ok(command)
}
