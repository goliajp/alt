# Sourced by the scripts that run a test command against a PostgreSQL of its
# own, with the test command as its arguments: sets `threads` to the tests the
# command runs at once. That is what `--test-threads` of the command says, or
# RUST_TEST_THREADS, or the processors this process may run on.
threads=''
told_threads=''
for argument in "$@"; do
    case "$argument" in
        --test-threads=*) threads=${argument#--test-threads=} ;;
        --test-threads) told_threads=1; continue ;;
    esac
    if [ -n "$told_threads" ]; then
        threads=$argument
        told_threads=''
    fi
done
case "$threads" in
    ''|*[!0-9]*) threads=${RUST_TEST_THREADS:-$(python3 -c 'import os; print(len(os.sched_getaffinity(0)) if hasattr(os, "sched_getaffinity") else os.cpu_count())')} ;;
esac
