#!/bin/sh
# Test gate — the single entrypoint for every test class.
#
# Class    What                                   When to run
#   lint   fmt --check + clippy -D warnings       before every commit
#   unit   per-crate library tests (--lib)        after every change
#   it     integration test binaries              before finishing a feature
#   corpus ignored corpus sweeps (.claude/corpus) at checkpoint exits
#   all    everything above                       at checkpoint exits
#
# Fast tier = default (non-ignored). Full tier adds the corpus class.
# Every #[ignore] must carry a reason string.
set -eu
cd "$(dirname "$0")/.."

class="${1:?usage: scripts/gate.sh lint|unit|it|corpus|all}"

run_lint() {
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
}

run_unit() {
    cargo test --workspace --lib
}

run_it() {
    cargo test --workspace --tests
}

run_corpus() {
    # absolute path: cargo test sets the package dir, not the workspace
    # root, as the tests' working directory
    ALT_CORPUS="${ALT_CORPUS:-$PWD/.claude/corpus}" \
        cargo test -p alt-git-codec --test it -- --ignored
}

case "$class" in
lint) run_lint ;;
unit) run_unit ;;
it) run_it ;;
corpus) run_corpus ;;
all)
    run_lint
    run_unit
    run_it
    run_corpus
    ;;
*)
    echo "unknown class: $class" >&2
    exit 2
    ;;
esac
echo "gate.sh $class: OK"
