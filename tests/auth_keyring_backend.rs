//! Keyring-backend lifecycle over the in-process MOCK store (lore.md 2.7;
//! runs only with `--features keyring`; the mock env var is honored only in
//! debug builds so a release user's store can never be silently swapped).
//!
//! **Layer:** L1 — deterministic, headless-safe.

use std::process::Command;

fn run(
    dir: &std::path::Path,
    home: &std::path::Path,
    args: &[&str],
    stdin: Option<&str>,
) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_libra"));
    cmd.args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("LIBRA_CONFIG_GLOBAL_DB", home.join(".libra/config.db"))
        .env("LIBRA_AUTH_KEYRING_MOCK", "1");
    if let Some(body) = stdin {
        use std::io::Write;
        let mut child = cmd
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(body.as_bytes())
            .expect("write");
        child.wait_with_output().expect("wait")
    } else {
        cmd.output().expect("run")
    }
}

/// NOTE: the mock store is per-process, so cross-INVOCATION persistence
/// cannot be asserted here — each libra run gets a fresh mock. What this
/// pins: the backend plumbing (config resolution, marker rows, status
/// states, migrate probe) and the never-echo invariant, headless.
#[test]
fn keyring_backend_plumbing_and_unreadable_state() {
    let dir = tempfile::tempdir().expect("dir");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let p = dir.path();
    // init a repo so global config machinery has its normal shape
    assert!(run(p, &home, &["init"], None).status.success());
    // Select the keyring backend.
    assert!(
        run(
            p,
            &home,
            &["config", "--global", "auth.backend", "keyring"],
            None
        )
        .status
        .success()
    );
    // login writes the OS entry (mock) + the marker row.
    let login = run(
        p,
        &home,
        &["auth", "login", "--host", "kr.example.com", "--with-token"],
        Some("mock-secret-token\n"),
    );
    assert!(
        login.status.success(),
        "{}",
        String::from_utf8_lossy(&login.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&login.stdout).contains("mock-secret-token"),
        "secret never echoed"
    );
    // A SECOND process sees the marker but a fresh (empty) mock store →
    // the row must surface as UNREADABLE, never as valid, and status --host
    // must exit non-zero.
    let status = run(p, &home, &["--json", "auth", "status"], None);
    assert!(status.status.success());
    let text = String::from_utf8_lossy(&status.stdout);
    assert!(text.contains("unreadable"), "marker without entry: {text}");
    assert!(
        !text.contains("mock-secret-token"),
        "secret never in JSON: {text}"
    );
    let hit = run(
        p,
        &home,
        &["auth", "status", "--host", "kr.example.com"],
        None,
    );
    assert_eq!(hit.status.code(), Some(1), "unreadable is not valid");
    // logout reaches BOTH backends (mock delete tolerated when absent).
    let logout = run(
        p,
        &home,
        &["auth", "logout", "--host", "kr.example.com"],
        None,
    );
    assert!(
        logout.status.success(),
        "{}",
        String::from_utf8_lossy(&logout.stderr)
    );
    let after = run(p, &home, &["--json", "auth", "status"], None);
    assert!(
        !String::from_utf8_lossy(&after.stdout).contains("unreadable"),
        "marker removed"
    );
    // migrate --to file with nothing readable moves 0 and flips the backend.
    let migrate = run(
        p,
        &home,
        &["--json", "auth", "migrate", "--to", "file"],
        None,
    );
    assert!(
        migrate.status.success(),
        "{}",
        String::from_utf8_lossy(&migrate.stderr)
    );
    let backend = run(
        p,
        &home,
        &["config", "--global", "get", "auth.backend"],
        None,
    );
    assert_eq!(
        String::from_utf8_lossy(&backend.stdout).trim(),
        "file",
        "backend flipped"
    );
}
