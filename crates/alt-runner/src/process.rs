use crate::{Error, Result, now_ns};
use alt_input::Context;
use alt_task::Termination;
use serde::{Deserialize, Serialize};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandObservation {
    pub argv: Vec<String>,
    pub executable: String,
    pub pid: u32,
    pub started_unix_ns: u64,
    pub finished_unix_ns: u64,
    pub termination: Termination,
    pub stdout_range: [u64; 2],
    pub stderr_range: [u64; 2],
}

pub(crate) fn wait(child: &mut Child, timeout: Duration) -> Result<(Termination, bool)> {
    let start = Instant::now();
    let group = child.id() as i32;
    loop {
        if let Some(status) = child.try_wait()? {
            // A shell that leaves children running has not finished validation.
            // Only this recorded child's newly created process group is touched.
            let descendants = unsafe { libc::kill(-group, 0) } == 0;
            if descendants {
                unsafe {
                    libc::kill(-group, libc::SIGKILL);
                }
            }
            let termination = if descendants {
                Termination::Unknown
            } else if let Some(code) = status.code() {
                Termination::Exit { code }
            } else if let Some(number) = status.signal() {
                Termination::Signal { number }
            } else {
                Termination::Unknown
            };
            return Ok((termination, descendants));
        }
        if start.elapsed() >= timeout {
            unsafe {
                libc::kill(-group, libc::SIGKILL);
            }
            child.wait()?;
            return Ok((Termination::Timeout, false));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub(crate) fn run(
    root: &Path,
    control: &Path,
    context: &Context,
    bindings: &BTreeMap<String, PathBuf>,
    timeout_ms: u64,
    problems: &mut Vec<String>,
) -> Result<(Termination, Vec<CommandObservation>)> {
    let cwd = fs::canonicalize(root.join(&context.cwd))?;
    if !cwd.starts_with(fs::canonicalize(root)?) || !cwd.is_dir() {
        return Err(Error::Invalid("command cwd escapes inputs".into()));
    }
    let start = Instant::now();
    let budget = Duration::from_millis(timeout_ms);
    let mut records = Vec::new();
    let mut final_status = Termination::Exit { code: 0 };
    for argv in &context.commands {
        let Some(binary) = bindings.get(&argv[0]) else {
            problems.push(format!("command_tool_unbound:{}", argv[0]));
            return Ok((Termination::Unknown, records));
        };
        let stdout = fs::OpenOptions::new()
            .append(true)
            .open(control.join("stdout.bin"))?;
        let stderr = fs::OpenOptions::new()
            .append(true)
            .open(control.join("stderr.bin"))?;
        let stdout_start = stdout.metadata()?.len();
        let stderr_start = stderr.metadata()?.len();
        let started = now_ns();
        let mut command = Command::new(binary);
        command
            .args(&argv[1..])
            .current_dir(&cwd)
            .env_clear()
            .envs(&context.environment.values)
            .stdin(Stdio::null())
            .stdout(stdout.try_clone()?)
            .stderr(stderr.try_clone()?)
            .process_group(0);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                problems.push(format!("command_spawn_failed:{error}"));
                return Ok((Termination::Unknown, records));
            }
        };
        let pid = child.id();
        fs::write(control.join("command.pid"), format!("{pid}\n"))?;
        let (termination, descendants) = wait(&mut child, budget.saturating_sub(start.elapsed()))?;
        if descendants {
            problems.push(format!("command_left_running_children:{pid}"));
        }
        records.push(CommandObservation {
            argv: argv.clone(),
            executable: binary.to_string_lossy().into_owned(),
            pid,
            started_unix_ns: started,
            finished_unix_ns: now_ns(),
            termination: termination.clone(),
            stdout_range: [stdout_start, stdout.metadata()?.len()],
            stderr_range: [stderr_start, stderr.metadata()?.len()],
        });
        final_status = termination;
        if final_status != (Termination::Exit { code: 0 }) {
            break;
        }
    }
    Ok((final_status, records))
}
