# alt for zsh: completion, and the state of the repository for the prompt.
#
# Load it with a plugin manager, `source` this file, or without any file
# through `eval "$(alt shell zsh)"`. See docs/shell.md.

0=${${ZERO:-${0:#$ZSH_ARGZERO}}:-${(%):-%N}}
0=${0:A:h}

fpath=("$0" "$0/functions" $fpath)
autoload -Uz _alt alt_prompt_setup alt_prompt_root alt_prompt_update \
  alt_prompt_arrived alt_prompt_info VCS_INFO_detect_alt VCS_INFO_get_data_alt \
  prompt_alt instant_prompt_alt +vi-alt-backend

alt_prompt_setup
