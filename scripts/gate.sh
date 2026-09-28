#!/bin/sh
# Test gate — the single entrypoint for every test class.
#
# Class    What                                   When to run
#   lint   fmt --check + clippy -D warnings       before every commit
#          + the LLM isolation check
#   unit   per-crate library tests (--lib)        after every change
#   it     integration test binaries              before finishing a feature
#   corpus ignored corpus sweeps (.dev/corpus)    at checkpoint exits
#   all    everything above                       at checkpoint exits
#
# Fast tier = default (non-ignored). Full tier adds the corpus class.
# Every #[ignore] must carry a reason string.
set -eu
cd "$(dirname "$0")/.."

class="${1:?usage: scripts/gate.sh lint|unit|it|corpus|all}"

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
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    run_isolation
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
    ALT_CORPUS="${ALT_CORPUS:-$PWD/.dev/corpus}" \
        cargo test --workspace --tests -- --ignored
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
