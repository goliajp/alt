mod limits;
mod pathological;
#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod regcomp;
