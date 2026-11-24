# Internal installation

The `0.1.0` package is a dogfood candidate, not a public release or a
promise that future repository formats can be downgraded. Public binary release
automation remains paused. Obtain an archive and its SHA-256 from its builder.

## Supported hosts

| Package | Task filesystem | Validation baseline |
| --- | --- | --- |
| `aarch64-apple-darwin` | local APFS | macOS 27.0.1, arm64 |
| `x86_64-unknown-linux-gnu` | local ext4 | Linux x86_64 with glibc; the actual OS/libc is in `manifest.json` |

The installer needs Python 3.11 or newer. Running the binaries does not require
Python. The packaged example also uses Python and `/bin/bash`. Independent Git
intake and delivery require an explicitly selected Git executable; tested tools
include Git 2.47.3 on Linux and 2.56.0 on macOS. This is a tested baseline, not
a compatibility claim for every older or newer toolchain. The Linux package is
not a static musl package. Neither Windows, Linux arm64, network filesystems nor
cloud-synchronized task directories are included in this internal support scope.

## Build an archive

From a source checkout, with Rust/Cargo and Python 3.11+ installed:

```sh
mkdir -p /absolute/path/to/packages
python3 scripts/package-internal.py \
  --output /absolute/path/to/packages/alt-internal.tar.gz
```

The output path must not exist. The command runs `cargo build --locked --release
--package alt-cli --bins`, checks the native executable architecture and CLI
version, then verifies that its source inputs did not change. Its JSON result
contains the archive SHA-256, release identity, version and target. Run builds on
your normal build host and respect its resource/benchmark scheduling rules.

The archive contains only `alt`, `altd`, `altd-server`, two licenses, README,
five guides and the server operator guide, the example, the installer and `manifest.json`. It contains no
repository, task records, validation logs or deployment configuration. The
manifest identifies the source inputs, lockfile, actual binaries, toolchain and
selected build settings. It does not claim that the build was hermetic or
reproducible across hosts. SHA-256 detects changes; it does not authenticate an
unknown publisher. Use a trusted archive and independently supplied digest.

## Verify and install

Verify the archive against the builder's digest before extracting it. For
example, on Linux use `sha256sum alt-internal.tar.gz`; on macOS use
`shasum -a 256 alt-internal.tar.gz`. Extract the trusted archive into a new
directory, preserving its recorded file modes:

```sh
mkdir unpacked
tar -xzpf alt-internal.tar.gz -C unpacked
```

Enter its `unpacked/alt-0.1.0-TARGET` directory:

```sh
python3 install.py verify
python3 install.py install --prefix "$HOME/.local/alt-internal"
export PATH="$HOME/.local/alt-internal/bin:$PATH"
export ALT_NO_DAEMON=1
alt --version
python3 "$HOME/.local/alt-internal/current/install.py" status \
  --prefix "$HOME/.local/alt-internal"
```

Create `$HOME/.local` first if needed. The prefix must be dedicated to this
installer and outside every repository. Installation does not edit shell startup
files, overwrite Cargo-installed tools, start a service or alter repositories.
Keep `ALT_NO_DAEMON=1` during initial verification and recovery so commands use
the selected binary directly. Existing daemons are not stopped by installation.
The normal client can refresh a daemon when its executable changes; the
installer itself does not make repository requests.

The package carries the zsh plugin under `share/alt/zsh/`. The result of
`install` has a `zsh` member with the line that loads it, to add to
`~/.zshrc` by hand; [alt in zsh](shell.md) describes what it does.

Files are copied and verified before activation. `current` is switched as one
symlink operation; `bin` resolves through it. The retained release directory is
named by the SHA-256 of its manifest. `status` verifies all retained releases,
and reports the active identity. A null active identity means initialization
was interrupted before activation; it is not a completed installation.

## Upgrade or select a retained build

Verify a new archive, extract it separately, and run its `install.py install`
with the same prefix. An identical package is idempotent. A different package
is added before `current` changes, and the previous release stays available.
Run `status`, `alt --version` and the [example](task-walkthrough.md) with the new
binary before using it for existing work.

```sh
python3 "$HOME/.local/alt-internal/current/install.py" activate \
  --prefix "$HOME/.local/alt-internal" --release RETAINED_MANIFEST_SHA256
```

Selecting an older executable does not undo repository writes and does not
establish on-disk downgrade compatibility. Before upgrading an existing
repository, preserve its complete private store and workspaces while writers
are stopped, and verify a separate Git exit for committed content. Use the
[recovery guide](recovery.md) to distinguish those two backups. The old unscoped
operation log stays readable; unsafe automatic undo is refused, not migrated by
guessing its workspace.

A store this build has imported into keeps the configuration of the Git
repository in a copy of its own, `.alt/git-import/source/`, beside the file
`alt config` writes. An older executable selected afterwards reads the store,
but two of its commands do not know the copy:

- its `alt export` writes a `.git/config` without what the source repository
  had configured (remotes, what each branch follows, attributes of the
  repository), and reports no error;
- its `alt import` into that store rewrites `.alt/git-import/config` from the
  Git repository: the values `alt config` set there since are lost.

Run `export` and a repeated `import` with this build or a newer one. Where an
older build has already exported, compare the exported `.git/config` with the
source repository's before relying on it.

A repository whose working tree or `.alt` directory belongs to another user is
refused, as Git refuses one (`detected dubious ownership`): its configuration
names filters that would be run. Name the directory in `safe.directory` of
your own Git configuration to open it. `altd-server` refuses to serve such a
repository to a fetch or a push alike, as `git daemon` does, and so does
`alt intake` to read one: where the server runs as another user than the one
the repositories belong to, name them in `safe.directory` of the configuration
that user reads: the working tree, or the control directory (`.alt`), as Git's
own servers want the `.git` directory named. The server opens the directory
it is asked for and no repository above it, a repository whose name begins
with a dot is not served, and an error it answers with names no path of the
server.

A repeated `alt import` goes by what the Git repository had when the store
last met it (the last import from it, or export to it), kept in
`.alt/git-import/met`. A branch, a tag, the notes or `HEAD` that Git has left
as they were then stay as the store has them, so commits made in the store
and not exported are no longer taken off their branch; where Git moved the
ref as well, Git's value is taken and the log of the ref leads back to the
store's, and the import names each branch it took off commits of the store,
with the commit it was at. A stash made in the store stays on top of the
stashes Git has, and one Git has dropped since goes. The record counts only
between the store and the Git repository it was made of: another store at the
same path, the store brought back from an older copy, or another repository
at the path of the first are told apart, and the import then takes every ref
Git has and drops no stash, as a first import does. So does the first import
after an upgrade from a build that kept the record without naming the two.

On macOS `alt init` and `alt import` set `core.precomposeUnicode` as
`git init` does, and file names are then recorded in their composed form.
A tree an older build committed there with decomposed names shows those files
as untracked afterwards, as it does to Git. A store an older build made has
no such setting: run `alt config core.precomposeUnicode true` in it once. The
names are composed as Git composes them, which is not Normalization Form C: a
name an earlier build of this release recorded in that form may differ in a
few code points and then shows as deleted beside an untracked file.

`alt revert`, `alt cherry-pick` and `alt merge` take `--continue`, `--skip`
(not `merge`), `--abort` and `--quit` as Git does, and refuse what Git
refuses: a revert gone on with after a commit of your own stops where the
index holds more than `HEAD`, and a change that is not staged is not written
over. `alt rebase --abort` now resets hard, as Git does: a change that is not
committed is gone with the rebase, and `alt undo` brings it back.
`alt rebase --skip` still refuses to drop a change that is not staged, and
`alt rebase --quit` puts `HEAD` back on the branch, since a store has no
`HEAD` off a branch.

If installation stops, inspect `status` before retrying. A complete staged
release with no active pointer can be activated by rerunning installation of
the same verified package. A malformed/incomplete prefix or unknown file is
preserved and refused; keep it for inspection and use a new prefix. Temporary
`.alt-install-*`, `.alt-activate-*` or `.alt-uninstall-*` directories may remain
in the prefix's parent after a killed process. Remove only a specific directory
whose installer process is known to have stopped and whose ownership/content
you have inspected; never remove them with a blanket wildcard.

## Uninstall

Stop using the selected binaries, then run:

```sh
python3 "$HOME/.local/alt-internal/current/install.py" uninstall \
  --prefix "$HOME/.local/alt-internal"
```

This verifies every owned release and removes only the dedicated installation.
Modified files, retargeted links and unknown entries cause refusal before
removal. Repositories, task receipts, Git exports, user configuration and other
installed tools remain separate. Remove the PATH entry from your own shell
configuration if you added one. Running processes are not terminated.
