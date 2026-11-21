# alt in zsh

The zsh plugin completes `alt` command lines and shows the state of the
repository in the prompt. It needs zsh 5.x and nothing else.

## Loading it

Any one of these, in `~/.zshrc`:

```zsh
# from the program alone: always the plugin that fits the installed alt
eval "$(alt shell zsh)"

# from the files, where an internal package installed them
source <prefix>/current/share/alt/zsh/alt.plugin.zsh

# with a plugin manager, from a checkout of this repository
#   oh-my-zsh:  ln -s <checkout>/shell/zsh $ZSH_CUSTOM/plugins/alt   and   plugins=(... alt)
#   zinit:      zinit light-mode for <checkout>/shell/zsh
#   antidote:   <checkout>/shell/zsh   in the plugins file
```

Completion needs the completion system (`autoload -Uz compinit && compinit`).
With `eval` or `source`, load the plugin after `compinit`; a plugin manager
that runs `compinit` itself finds `_alt` on `fpath`. To have only the
completion, write `alt shell zsh --completion` to a file named `_alt` in a
directory on `fpath`.

The internal installer prints the `source` line for its prefix as the `zsh`
member of its result. It changes no file of yours.

## Completion

Tab completes subcommands and options with their descriptions, the values an
option lists, and what the repository has for the word at the cursor:

| where | what is offered |
|---|---|
| `alt switch`, `alt merge`, `alt branch -d` | branches (`-r`: remote branches) |
| `alt show`, `alt log`, `alt rebase`, `alt revert`, `alt cherry-pick`, ... | branches, tags, remote branches, `HEAD`, and commits by the start of their id (4 digits or more) |
| `alt tag -d` | tags |
| `alt fetch`, `alt push`, `alt remote remove` | remotes, with their URLs |
| `alt push <remote>` | branches and notes refs; behind `src:`, the branches of that remote |
| `alt add` | paths in conflict, changed paths, untracked paths |
| `alt blame` | tracked paths |
| `alt config` | the keys that are set and the keys alt reads |
| `alt flow feature finish` (and `release`, `hotfix`) | the open topics |
| `--workspace`, `alt workspace remove` | workspaces, with their directory and branch |
| directories and files (`alt init`, `alt clone`, ...) | zsh's own file completion |

The candidates come in groups named `commands`, `options`, `values`,
`branches`, `tags`, `remote-branches`, `commits`, `refs`, `remotes`,
`workspaces`, `modified-files`, `untracked-files`, `unmerged-files`,
`tracked-files` and `config-keys`. They are completion tags, so the usual
styles apply:

```zsh
zstyle ':completion:*:*:alt:*' group-order branches tags remote-branches commits
zstyle ':completion:*:*:alt:*:descriptions' format '%B%d%b'
```

More than 1000 paths are offered a directory level at a time.

## The prompt

The plugin keeps the associative array `alt_prompt` current before every
prompt. It asks `alt prompt` in the background and redraws the prompt when
the answer arrives, so the prompt is never held back; until then it shows
what it showed last. Outside a repository nothing is run at all.

There are three ways to show it.

### `alt_prompt_info`

```zsh
setopt prompt_subst
PROMPT='%~ $(alt_prompt_info) %# '
```

or let the plugin put it in `RPROMPT`:

```zsh
zstyle ':alt:prompt' auto yes     # before the plugin is loaded
```

What it prints follows the prompt that comes with Git:

```
develop                 clean
develop *+%             * unstaged   + staged   % untracked
develop|MERGING !1      a merge under way, 1 path in conflict
feature/x|REBASE 3/7    also CHERRY-PICKING, REVERTING, BISECTING
(6d1f3128)              HEAD on no branch; a tag at that commit instead of the id
develop ↑2↓1            2 commits ahead of the remote's branch, 1 behind
develop [side]          in the parallel workspace `side`
develop ⚠               the repository needs attention (see `alt prompt` below)
```

| style | under | meaning | default |
|---|---|---|---|
| `async` | `:alt:prompt` | ask in the background | yes |
| `auto` | `:alt:prompt` | set `RPROMPT` | no |
| `untracked` | `:alt:prompt` | look for untracked files | yes |
| `ahead-behind` | `:alt:prompt` | count commits against the remotes | yes |
| `remote` | `:alt:prompt` | the remote to compare with, or `all` | the first by name |
| `color` | `:alt:prompt` | print in colour | yes |
| `format` | `:alt:prompt` | `%b` branch, `%o` operation, `%f` flags, `%u` unmerged, `%r` ahead and behind, `%w` workspace, `%a` attention | `%b%o%f%u%r%w%a` |
| `unstaged`, `staged`, `untracked`, `unmerged`, `ahead`, `behind`, `attention` | `:alt:prompt:symbols` | the symbol of that part | `*` `+` `%` `!` `↑` `↓` `⚠` |
| `branch`, `operation`, `flags`, `unmerged`, `remote`, `workspace`, `attention` | `:alt:prompt:colors` | the colour of that part, empty for none | green, yellow, red, red, cyan, blue, red |

`alt_prompt_info plain` prints without colours. `alt_prompt_update now`
brings `alt_prompt` up to date and waits for it, for use in scripts.

### vcs_info

A theme that shows `vcs_info` shows alt repositories with no change: the
plugin adds `alt` to its backends. `%b` is the branch, `%a` the operation
under way, `%c` and `%u` staged and unstaged changes (with the
`check-for-changes` style), `%m` the commits ahead of and behind the first
remote (`+2-1`).

### powerlevel10k

Add `alt` to `POWERLEVEL9K_LEFT_PROMPT_ELEMENTS` (or the right ones). The
segment has the states `CLEAN`, `MODIFIED`, `UNTRACKED` and `CONFLICTED`, so
it is coloured like p10k's own `vcs` segment:
`POWERLEVEL9K_ALT_MODIFIED_FOREGROUND` and so on.

## `alt prompt`

`alt prompt [--format=lines|zsh] [--no-untracked] [--no-ahead-behind]` is what
the plugin asks, and is there for any other prompt to ask. Outside a
repository it prints nothing and exits with 1. In one it exits with 0 and
never writes to stderr. With `--format=lines` (the default) each line is a
key and its value; the keys come in this order, and a key with nothing to say
is left out:

```
repo /path/to/root
workspace default
branch develop          or `detached 6d1f3128`, or `unborn main` before the first commit
tag v0.1.0              HEAD is on no branch and at the commit of this tag
operation rebase        merge, rebase, cherry-pick, revert or bisect
step 3 7                which of how many commits the operation is at
staged 2                paths; a path in conflict counts only as unmerged
unstaged 1
untracked 4
unmerged 1
remote origin 2 0       for each remote that has a branch of this name: commits ahead, behind
attention unconfirmed   or `recovery`, or `damaged`
```

A count ends in `+` when more than 10000 commits lie between the two
branches and the count was cut there. `attention` is `unconfirmed` when the
last operations of the log were never synced to disk, `recovery` when an
operation waits for `alt operation recover`, and `damaged` when part of the
repository could not be read.

`--format=zsh` prints one line for zsh to evaluate,
`typeset -gA alt_prompt=('key' 'value' ...)`: each line under its key, a
remote under `remote:<name>` with the value `'<ahead> <behind>'`, and the
names of the remotes under `remotes`.

Keys may be added in later versions; the meaning and the order of these do
not change.

## `alt complete`

`alt complete --shell zsh --current <N> -- <word>...` is what the completion
function asks: the words of the command line, the program first, and which of
them the cursor is in, counted from 0. Asked this way it exits with 0 and
never writes to stderr; outside a repository it answers what the command line
itself says. The answer is lines of fields separated by tabs:

```
group<TAB>name<TAB>title    the candidates that follow belong to this group
value<TAB>description       a candidate; the description may be empty
files<TAB>file|dir          the shell completes file names (or directories) itself
nospace                     no space follows the candidates since the last group line
prefix<TAB>text             what follows stands behind this text in the word
```

In a field a tab is written `\t`, a line break `\n` and a backslash `\\`. A
candidate spelled `group`, `files`, `nospace` or `prefix` has a backslash
behind its first letter; a backslash before any other character stands for
that character.
