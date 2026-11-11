//! Direct observer entrypoints also work outside repositories. All machine
//! failures use the ordinary CLI result envelope; raw command output stays in
//! the observer's files and is never mixed into its JSON response.
use crate::cli::RunnerOp;
type Res<T> = Result<T, Box<dyn std::error::Error>>;
use alt_json::json;
use std::io::Write;

pub fn run(op: &RunnerOp, out: &mut impl Write) -> Res<()> {
    let root = std::env::current_dir()?;
    let value = match op {
        RunnerOp::Probe {
            bindings,
            environment,
        } => {
            let bindings: Vec<alt_runner::ToolBinding> = alt_json::from_str(bindings)
                .map_err(|error| crate::machine::Failure::usage(error.to_string()))?;
            let environment: std::collections::BTreeMap<String, String> =
                alt_json::from_str(environment)
                    .map_err(|error| crate::machine::Failure::usage(error.to_string()))?;
            let tools = bindings
                .iter()
                .map(|binding| alt_runner::probe(&root, binding, &environment))
                .collect::<alt_runner::Result<Vec<_>>>()?;
            let observer = std::fs::read(std::env::current_exe()?)?;
            json!({"schema_version":1,"runner":alt_runner::host_identity()?,"tools":tools,"observer_blake3":alt_hash::blake3::hash(&observer).to_hex().to_string()})
        }
        RunnerOp::Execute { spec } => {
            let spec: alt_runner::Spec = alt_json::from_slice(&std::fs::read(spec)?)?;
            use std::io::Read;
            let mut authentication_key = [0; 32];
            std::io::stdin()
                .lock()
                .read_exact(&mut authentication_key)?;
            alt_json::to_value(alt_runner::execute(&root, &spec, &authentication_key)?)?
        }
    };
    alt_json::to_writer(&mut *out, &value)?;
    writeln!(out)?;
    Ok(())
}
