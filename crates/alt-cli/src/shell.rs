//! `alt shell zsh`: the zsh plugin, as this build of alt carries it.
//!
//! The files under `shell/zsh/` are the plugin as a plugin manager loads
//! it. They are compiled in, so that `eval "$(alt shell zsh)"` loads the
//! same plugin from the program alone, always the one that fits it.

use crate::daemon::Response;

#[derive(alt_args::Args, Clone)]
pub struct ShellArgs {
    /// The shell to print the plugin for
    #[arg(value_parser = ["zsh"], hint = none)]
    pub shell: String,
    /// Print only the completion function, as the file `_alt` to put on
    /// `fpath`
    #[arg(long)]
    pub completion: bool,
}

const COMPLETION: &str = include_str!("../../../shell/zsh/_alt");

/// The functions of the plugin, each the body of the function its file
/// is named after.
const FUNCTIONS: [(&str, &str); 11] = [
    ("_alt", COMPLETION),
    (
        "alt_prompt_setup",
        include_str!("../../../shell/zsh/functions/alt_prompt_setup"),
    ),
    (
        "alt_prompt_root",
        include_str!("../../../shell/zsh/functions/alt_prompt_root"),
    ),
    (
        "alt_prompt_update",
        include_str!("../../../shell/zsh/functions/alt_prompt_update"),
    ),
    (
        "alt_prompt_arrived",
        include_str!("../../../shell/zsh/functions/alt_prompt_arrived"),
    ),
    (
        "alt_prompt_info",
        include_str!("../../../shell/zsh/functions/alt_prompt_info"),
    ),
    (
        "VCS_INFO_detect_alt",
        include_str!("../../../shell/zsh/functions/VCS_INFO_detect_alt"),
    ),
    (
        "VCS_INFO_get_data_alt",
        include_str!("../../../shell/zsh/functions/VCS_INFO_get_data_alt"),
    ),
    (
        "+vi-alt-backend",
        include_str!("../../../shell/zsh/functions/+vi-alt-backend"),
    ),
    (
        "prompt_alt",
        include_str!("../../../shell/zsh/functions/prompt_alt"),
    ),
    (
        "instant_prompt_alt",
        include_str!("../../../shell/zsh/functions/instant_prompt_alt"),
    ),
];

/// What `alt shell zsh` prints: every function defined, then the plugin
/// set up; with `completion`, the completion function alone.
pub fn script(completion: bool) -> String {
    if completion {
        return COMPLETION.to_owned();
    }
    let mut text = String::new();
    for (name, body) in FUNCTIONS {
        text += &format!("{name}() {{\n{}\n}}\n", body.trim_end());
    }
    text + "alt_prompt_setup\n"
}

pub fn respond(args: &ShellArgs) -> Response {
    Response {
        exit_code: 0,
        stdout: script(args.completion).into_bytes(),
        stderr: Vec::new(),
    }
}
