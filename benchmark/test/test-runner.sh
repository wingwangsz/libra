#!/usr/bin/env bash

set -euo pipefail

BENCHMARK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly BENCHMARK_DIR
readonly RUNNER="$BENCHMARK_DIR/run.sh"
declare -a RUNNER_BINARY_ARGS=()

if [[ -n "${LIBRA_BENCHMARK_TEST_BINARY:-}" ]]; then
    RUNNER_BINARY_ARGS=(--binary "$LIBRA_BENCHMARK_TEST_BINARY")
fi

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

assert_contains() {
    local needle="$1"
    local haystack="$2"
    [[ "$haystack" == *"$needle"* ]] || fail "expected output to contain: $needle"
}

assert_valid_json() {
    perl -MJSON::PP -0777 -e 'decode_json(<>);' "$1" >/dev/null \
        || fail "result is not valid JSON: $1"
}

test_help_documents_revision_selection() {
    local output
    output="$($RUNNER --help)"
    assert_contains "--revision" "$output"
    assert_contains "--binary" "$output"
    assert_contains "--control-binary" "$output"
}

test_scenarios_write_machine_readable_result() {
    local output_dir result canonical_result output
    output_dir="$(mktemp -d "${TMPDIR:-/tmp}/libra-benchmark-test.XXXXXX")"
    trap 'rm -rf "$output_dir"' RETURN

    output="$($RUNNER \
        "${RUNNER_BINARY_ARGS[@]}" \
        --scenario status_clean \
        --scenario status_dirty \
        --file-count 2 \
        --runs 1 \
        --warmup 0 \
        --output "$output_dir/result.json")"
    result="$output_dir/result.json"
    canonical_result="$(cd "$(dirname "$result")" && pwd)/$(basename "$result")"

    [[ -f "$result" ]] || fail "benchmark result was not written"
    assert_valid_json "$result"
    assert_contains '"scenario":"status_clean"' "$(tr -d '[:space:]' < "$result")"
    assert_contains '"scenario":"status_dirty"' "$(tr -d '[:space:]' < "$result")"
    assert_contains '"samples":1' "$(tr -d '[:space:]' < "$result")"
    assert_contains "$canonical_result" "$output"
}

test_help_documents_revision_selection
test_scenarios_write_machine_readable_result
printf 'PASS: benchmark runner contract\n'
