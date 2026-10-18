#!/bin/sh
# Test gate — the single entrypoint for every test class.
#
# Class    What                                   When to run
#   lint   fmt --check + clippy -D warnings       before every commit
#          + the LLM isolation check
#   unit   per-crate library tests (--lib)        after every change
#   it     integration test binaries              before finishing a feature
#   corpus ignored corpus sweeps (.dev/corpus)    at checkpoint exits
#   package internal installer boundaries        after packaging changes
#   all    everything above                       at checkpoint exits
#
# Fast tier = default (non-ignored). Full tier adds the corpus class.
# Every #[ignore] must carry a reason string.
set -eu
cd "$(dirname "$0")/.."

usage() {
    cat <<'EOF'
Usage: scripts/gate.sh <lint|unit|it|corpus|package|all> [options]
  -p, --package NAME   Check one package (default: the entire workspace)
      --workspace      Explicitly check the entire workspace
      --test NAME      Select an integration target (it/corpus, requires -p)
      --filter NAME    Select matching tests (unit/it/corpus, requires -p)
      --exact          Match the full test name (requires --filter)
      --release        Use the release profile
  -h, --help           Show this help

Examples:
  scripts/gate.sh lint -p alt-cli
  scripts/gate.sh it -p alt-cli --test it --filter workspace::
  scripts/gate.sh unit -p alt-refs --filter tests::example --exact

Scoped lint formats and compiles only the selected package; the read-only
LLM isolation policy still checks the whole repository. Unknown arguments
are errors. The package class takes no scope/profile options and does not build
Rust code. Workspace/release checks belong on a remote runner.
EOF
}

die() { echo "gate.sh: $*" >&2; exit 2; }
value() {
    [ -n "$2" ] || die "$1 needs a value"
    case "$2" in -*) die "$1 needs a value, got $2" ;; esac
}

[ "$#" -gt 0 ] || { usage >&2; exit 2; }
class=$1
shift
case "$class" in
    -h|--help) [ "$#" -eq 0 ] || die "unexpected arguments after help"; usage; exit 0 ;;
    lint|unit|it|corpus|package|all) ;;
    *) die "unknown class: $class" ;;
esac
package='' scope='' target='' filter='' exact='' release='' help=''
while [ "$#" -gt 0 ]; do
    case "$1" in
        -p|--package)
            [ -z "$scope" ] || die "specify --package or --workspace only once"
            value "$1" "${2-}"
            case "$2" in *[!a-zA-Z0-9_-]*) die "--package needs one literal package name" ;; esac
            package=$2; scope=package; shift ;;
        --workspace)
            [ -z "$scope" ] || die "specify --package or --workspace only once"
            scope=workspace ;;
        --test)
            [ -z "$target" ] || die "duplicate --test"
            value "$1" "${2-}"; target=$2; shift ;;
        --filter)
            [ -z "$filter" ] || die "duplicate --filter"
            value "$1" "${2-}"; filter=$2; shift ;;
        --exact) [ -z "$exact" ] || die "duplicate --exact"; exact=1 ;;
        --release) [ -z "$release" ] || die "duplicate --release"; release=1 ;;
        -h|--help) help=1 ;;
        *) die "unknown argument: $1" ;;
    esac
    shift
done
if [ -n "$target$filter" ]; then
    [ -n "$package" ] || die "--test/--filter require --package"
    case "$class" in lint|all) die "--test/--filter are not valid for $class" ;; esac
fi
if [ "$class" = package ] && [ -n "$scope$target$filter$exact$release" ]; then
    die "package does not accept scope, test, filter or profile options"
fi
[ -z "$target" ] || [ "$class" != unit ] || die "--test is not valid for unit"
[ -z "$exact" ] || [ -n "$filter" ] || die "--exact requires --filter"
[ -z "$help" ] || { usage; exit 0; }

# Build argv without eval or unquoted user input. Keep the scope in one place
# so every class uses the same package selection.
scoped_cargo() {
    command=$1
    shift
    if [ -n "$package" ]; then
        set -- "$command" --package "$package" "$@"
    elif [ "$command" = fmt ]; then
        set -- "$command" --all "$@"
    else
        set -- "$command" --workspace "$@"
    fi
    cargo "$@"
}

# The core never talks to a language model: no LLM, tokenizer or
# embedding crate anywhere in the dependency graph, and the only crates
# allowed an HTTP client are the protocol transports (git wire, LFS). A
# future LLM integration lives in its own opt-in crate, `alt-llm-bridge`,
# which is the one exemption on both counts.
LLM_CRATES='async-openai openai openai-api-rs anthropic anthropic-sdk
misanthropy tiktoken-rs tokenizers llm llm-chain ollama-rs rust-bert
candle-core candle-transformers ort fastembed genai langchain-rust'
HTTP_CRATES='reqwest hyper ureq isahc surf attohttpc curl awc'
HTTP_OWNERS='alt-wire-http alt-lfs alt-llm-bridge'
LLM_SOURCE='\b(openai|anthropic|claude|gemini|cohere|mistral|ollama|tiktoken|huggingface)\b|\bllm_|\buse llm\b|api\.openai\.com|api\.anthropic\.com|generativelanguage\.googleapis\.com'

# workspace members that depend on $1, directly when $2 = 1
dependents() {
    cargo tree -i "$1" --workspace --prefix none ${2:+--depth "$2"} 2>/dev/null |
        sed -n "s|^\([a-z0-9_-]*\) v[^ ]* ($PWD/.*|\1|p" | sort -u
}

run_isolation() {
    fail=0
    # probe: the one known HTTP edge must show up, or cargo tree itself
    # failed and every check below would pass vacuously
    if ! dependents ureq 1 | grep -qx alt-wire-http; then
        echo "llm isolation: cannot read the dependency graph" >&2
        return 1
    fi
    for c in $LLM_CRATES; do
        for m in $(dependents "$c"); do
            [ "$m" = alt-llm-bridge ] && continue
            echo "llm isolation: $m pulls in $c" >&2
            fail=1
        done
    done
    for c in $HTTP_CRATES; do
        for m in $(dependents "$c" 1); do
            case " $HTTP_OWNERS " in *" $m "*) continue ;; esac
            echo "llm isolation: $m depends on HTTP client $c" >&2
            fail=1
        done
    done
    if grep -rniE "$LLM_SOURCE" crates fuzz --include='*.rs' --include=Cargo.toml \
        --exclude-dir=alt-llm-bridge --exclude-dir=target >&2; then
        echo "llm isolation: LLM reference in core source (above)" >&2
        fail=1
    fi
    [ "$fail" = 0 ]
}

run_lint() {
    scoped_cargo fmt --check
    set -- --all-targets
    [ -z "$release" ] || set -- "$@" --release
    scoped_cargo clippy "$@" -- -D warnings
    run_isolation
}

run_tests() {
    kind=$1
    if [ "$kind" = unit ]; then
        set -- --lib
    elif [ -n "$target" ]; then
        set -- --test "$target"
    else
        set -- --tests
    fi
    [ -z "$release" ] || set -- "$@" --release
    set -- "$@" --
    [ "$kind" != corpus ] || set -- "$@" --ignored
    [ -z "$exact" ] || set -- "$@" --exact
    [ -z "$filter" ] || set -- "$@" "$filter"
    scoped_cargo test "$@"
}

run_corpus() {
    # absolute path: cargo test sets the package dir, not the workspace
    # root, as the tests' working directory
    (
        ALT_CORPUS="${ALT_CORPUS:-$PWD/.dev/corpus}"
        # Resolve overrides too, before Cargo changes the working directory.
        ALT_CORPUS=$(cd "$ALT_CORPUS" && pwd -P) || exit 2
        export ALT_CORPUS
        run_tests corpus
    )
}

run_package() {
    python3 scripts/test-package.py
}

case "$class" in
lint) run_lint ;;
unit) run_tests unit ;;
it) run_tests it ;;
corpus) run_corpus ;;
package) run_package ;;
all)
    run_lint
    run_tests unit
    case "$package" in ''|alt-cli) run_package ;; esac
    run_tests it
    run_corpus
    ;;
*)
    echo "unknown class: $class" >&2
    exit 2
    ;;
esac
echo "gate.sh $class: OK"
