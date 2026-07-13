//! Integration tests for the `libra hooks --help` surface.
//!
//! **Layer:** L1 — deterministic, no external dependencies. Covers
//! the cross-cutting `--help` EXAMPLES rollout from
//! `docs/development/commands/_general.md` item B for the AI agent hook entry
//! point command.

use super::*;

/// `libra hooks --help` surfaces the EXAMPLES banner so operators see
/// the most commonly wired Claude / Gemini lifecycle events
/// (session-start, prompt, tool-use, stop, session-end) without
/// reading the design doc.
#[test]
fn test_hooks_help_lists_examples_banner() {
    let repo = tempdir().expect("tempdir for hooks --help");
    let output = run_libra_command(&["hooks", "--help"], repo.path());
    assert!(
        output.status.success(),
        "hooks --help should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("EXAMPLES:"),
        "hooks --help should include EXAMPLES banner, stdout: {stdout}"
    );
    for invocation in [
        "libra hooks claude session-start",
        "libra hooks claude prompt",
        "libra hooks claude tool-use",
        "libra hooks claude stop",
        "libra hooks claude session-end",
        // AG-19: gemini is uninstall-only (single reject line) and codex
        // is the new stable installed surface.
        "libra hooks codex session-start",
        "libra hooks codex stop",
        "libra hooks codex subagent-start",
        "libra hooks gemini <event>",
        "libra agent remove gemini",
    ] {
        assert!(
            stdout.contains(invocation),
            "hooks --help should include `{invocation}`, stdout: {stdout}"
        );
    }
}
