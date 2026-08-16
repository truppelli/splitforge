#!/usr/bin/env bash
#
# Every gate in .github/workflows/ci.yml, in the same order, with the same commands.
#
# If a command here stops matching the workflow, this script is wrong — the workflow is the
# specification. Keeping them literally identical is the point: a local run that is
# "basically the same" is a local run that passes while CI fails.
#
# Usage:  splitforge-ci [gate ...]     (default: all)
#         gates: fmt clippy test msrv audit deny cross

set -uo pipefail

MSRV="${MSRV:-1.88.0}"
FAILED=()
PASSED=()

run_gate() {
    local name="$1"
    shift
    printf '\n\033[1m━━━ %s ━━━\033[0m\n' "$name"
    printf '  $ %s\n\n' "$*"
    if "$@"; then
        PASSED+=("$name")
        printf '\n\033[32m✓ %s\033[0m\n' "$name"
    else
        FAILED+=("$name")
        printf '\n\033[31m✗ %s\033[0m\n' "$name"
    fi
}

gate_fmt() {
    run_gate "Format" cargo fmt --all --check
}

gate_clippy() {
    run_gate "Clippy" \
        cargo clippy --workspace --all-targets --all-features -- -D warnings
}

gate_test() {
    run_gate "Test" cargo test --workspace --all-features
}

gate_msrv() {
    run_gate "MSRV (${MSRV})" \
        cargo "+${MSRV}" check --workspace --all-features --all-targets
}

gate_audit() {
    run_gate "Security advisories" cargo audit --deny warnings
}

gate_deny() {
    # `--all-features` is a global option, so it goes before the subcommand.
    run_gate "Licenses and bans" cargo deny --all-features check
}

gate_cross() {
    # The gate this image exists for. The whole workspace, not just the edge service:
    # the edge service is still empty, and a cross-build that compiles nothing proves
    # nothing.
    run_gate "Cross-build (aarch64)" \
        cargo build --release --target aarch64-unknown-linux-gnu --workspace
}

gates=("$@")
if [ "${#gates[@]}" -eq 0 ] || [ "${gates[0]}" = "all" ]; then
    gates=(fmt clippy test msrv audit deny cross)
fi

for gate in "${gates[@]}"; do
    case "$gate" in
        fmt)    gate_fmt ;;
        clippy) gate_clippy ;;
        test)   gate_test ;;
        msrv)   gate_msrv ;;
        audit)  gate_audit ;;
        deny)   gate_deny ;;
        cross)  gate_cross ;;
        *)
            printf '\033[31munknown gate: %s\033[0m\n' "$gate" >&2
            printf 'known gates: fmt clippy test msrv audit deny cross\n' >&2
            exit 2
            ;;
    esac
done

printf '\n\033[1m━━━ summary ━━━\033[0m\n'
for gate in "${PASSED[@]:-}"; do
    [ -n "$gate" ] && printf '\033[32m  ✓ %s\033[0m\n' "$gate"
done
for gate in "${FAILED[@]:-}"; do
    [ -n "$gate" ] && printf '\033[31m  ✗ %s\033[0m\n' "$gate"
done

if [ "${#FAILED[@]}" -gt 0 ]; then
    printf '\n\033[31m%d gate(s) failed.\033[0m\n' "${#FAILED[@]}"
    exit 1
fi
printf '\n\033[32mAll %d gates passed.\033[0m\n' "${#PASSED[@]}"
