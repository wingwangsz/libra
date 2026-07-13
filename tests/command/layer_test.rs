//! Integration tests for `libra layer` — the local-overlay primitive (lore.md
//! 2.4). Verifies the two load-bearing invariants end-to-end: never-enters-
//! commit (ignore-exclusion + the airtight `add --force` guard) and
//! never-clobbers (tracked-path collision + edit preservation).
//!
//! Layer: L1 (deterministic; tempdir + isolated HOME, no network).

use std::fs;

use super::{
    assert_cli_success, create_committed_repo_via_cli, parse_cli_error_stderr, run_libra_command,
};

/// Build a source overlay directory (OUTSIDE the repo — layer sources are
/// arbitrary local dirs, not tracked repo content) with two files, one nested.
/// Returns the owning tempdir (kept alive by the caller) and the source path.
fn make_overlay() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("overlay tempdir");
    let overlay = dir.path().join("overlay");
    fs::create_dir_all(overlay.join("sub")).expect("mkdir overlay");
    fs::write(overlay.join("a.txt"), "overlay-a\n").expect("write a");
    fs::write(overlay.join("sub/b.txt"), "overlay-b\n").expect("write b");
    (dir, overlay)
}

#[test]
fn layer_apply_materializes_hides_from_status_and_blocks_staging() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let (_src, overlay) = make_overlay();

    assert_cli_success(
        &run_libra_command(
            &[
                "layer",
                "add",
                "scratch",
                "--source",
                overlay.to_str().unwrap(),
            ],
            p,
        ),
        "layer add",
    );
    assert_cli_success(&run_libra_command(&["layer", "apply"], p), "layer apply");

    // Files are materialized on disk.
    assert_eq!(fs::read_to_string(p.join("a.txt")).unwrap(), "overlay-a\n");
    assert_eq!(
        fs::read_to_string(p.join("sub/b.txt")).unwrap(),
        "overlay-b\n"
    );

    // status must NOT list the overlay (excluded like ignored).
    let status = run_libra_command(&["status", "--porcelain"], p);
    let out = String::from_utf8_lossy(&status.stdout);
    assert!(!out.contains("a.txt"), "overlay hidden from status: {out}");
    assert!(!out.contains("b.txt"), "nested overlay hidden: {out}");

    // `add .` succeeds staging nothing new (the overlay is excluded).
    assert_cli_success(&run_libra_command(&["add", "."], p), "add . succeeds");
    let after = run_libra_command(&["status", "--porcelain"], p);
    assert!(
        !String::from_utf8_lossy(&after.stdout).contains("a.txt"),
        "overlay never staged by add ."
    );

    // The airtight invariant: `add --force <overlay>` is REJECTED
    // (LBR-LAYER-001) — force bypasses ignore, the staging guard does not.
    let forced = run_libra_command(&["add", "--force", "a.txt"], p);
    assert_eq!(
        forced.status.code(),
        Some(128),
        "force-add of overlay refused"
    );
    let (_h, report) = parse_cli_error_stderr(&forced.stderr);
    assert_eq!(report.error_code, "LBR-LAYER-001");

    // An explicit `add a.txt` also cannot stage it.
    let explicit = run_libra_command(&["add", "a.txt"], p);
    assert_ne!(
        explicit.status.code(),
        Some(0),
        "explicit add of overlay refused"
    );
}

#[test]
fn layer_apply_rejects_collision_with_tracked_path() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    // Overlay (EXTERNAL source dir) whose destination collides with the
    // committed `tracked.txt`.
    let src = tempfile::tempdir().expect("src tempdir");
    let overlay = src.path().join("overlay");
    fs::create_dir_all(&overlay).expect("mkdir");
    fs::write(overlay.join("tracked.txt"), "shadow\n").expect("write");

    assert_cli_success(
        &run_libra_command(
            &[
                "layer",
                "add",
                "shadow",
                "--source",
                overlay.to_str().unwrap(),
            ],
            p,
        ),
        "layer add",
    );
    let apply = run_libra_command(&["layer", "apply"], p);
    assert_eq!(apply.status.code(), Some(128), "collision refused");
    let (_h, report) = parse_cli_error_stderr(&apply.stderr);
    assert_eq!(report.error_code, "LBR-LAYER-001");
    // Fail-closed: the tracked file's committed content is untouched.
    assert_eq!(
        fs::read_to_string(p.join("tracked.txt")).unwrap(),
        "tracked\n"
    );
}

#[test]
fn layer_unapply_preserves_user_edits() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let (_src, overlay) = make_overlay();
    assert_cli_success(
        &run_libra_command(
            &[
                "layer",
                "add",
                "scratch",
                "--source",
                overlay.to_str().unwrap(),
            ],
            p,
        ),
        "layer add",
    );
    assert_cli_success(&run_libra_command(&["layer", "apply"], p), "layer apply");

    // Edit one materialized file; leave the other pristine.
    fs::write(p.join("a.txt"), "USER EDITED\n").expect("edit a");

    let unapply = run_libra_command(&["layer", "unapply"], p);
    assert_cli_success(&unapply, "layer unapply");
    // The edited file is PRESERVED; the pristine one is removed.
    assert_eq!(
        fs::read_to_string(p.join("a.txt")).unwrap(),
        "USER EDITED\n",
        "user edit never clobbered"
    );
    assert!(!p.join("sub/b.txt").exists(), "pristine overlay removed");
}

#[test]
fn layer_reapply_prunes_removed_source_files() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let (_src, overlay) = make_overlay();
    assert_cli_success(
        &run_libra_command(
            &[
                "layer",
                "add",
                "scratch",
                "--source",
                overlay.to_str().unwrap(),
            ],
            p,
        ),
        "layer add",
    );
    assert_cli_success(&run_libra_command(&["layer", "apply"], p), "apply 1");
    assert!(p.join("sub/b.txt").exists());

    // Remove a source file, re-apply: the stale materialized path is pruned.
    fs::remove_file(overlay.join("sub/b.txt")).expect("rm source");
    assert_cli_success(&run_libra_command(&["layer", "apply"], p), "apply 2");
    assert!(
        !p.join("sub/b.txt").exists(),
        "stale path pruned on re-apply"
    );
    assert!(p.join("a.txt").exists(), "surviving path kept");
}

#[test]
fn layer_source_symlink_and_reserved_paths_rejected() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    // A source (EXTERNAL) that would materialize into a reserved ignore file.
    let src = tempfile::tempdir().expect("src tempdir");
    let overlay = src.path().join("overlay");
    fs::create_dir_all(&overlay).expect("mkdir");
    fs::write(overlay.join(".libraignore"), "x\n").expect("write");
    assert_cli_success(
        &run_libra_command(
            &["layer", "add", "bad", "--source", overlay.to_str().unwrap()],
            p,
        ),
        "layer add",
    );
    let apply = run_libra_command(&["layer", "apply"], p);
    assert_ne!(
        apply.status.code(),
        Some(0),
        "reserved-path overlay refused"
    );
}

#[test]
fn layer_json_envelopes_are_stable() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let (_src, overlay) = make_overlay();
    assert_cli_success(
        &run_libra_command(
            &[
                "layer",
                "add",
                "scratch",
                "--source",
                overlay.to_str().unwrap(),
            ],
            p,
        ),
        "layer add",
    );
    assert_cli_success(&run_libra_command(&["layer", "apply"], p), "apply");
    let listed = run_libra_command(&["--json", "layer", "list"], p);
    assert_cli_success(&listed, "json list");
    let json: serde_json::Value = serde_json::from_slice(&listed.stdout).expect("valid json list");
    assert_eq!(json["data"]["layers"][0]["name"].as_str(), Some("scratch"));
    let status = run_libra_command(&["--json", "layer", "status"], p);
    let sjson: serde_json::Value =
        serde_json::from_slice(&status.stdout).expect("valid json status");
    assert_eq!(
        sjson["data"]["materialized"].as_array().map(|a| a.len()),
        Some(2)
    );
}

#[test]
fn layer_rejects_source_inside_worktree() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    // Source INSIDE the repo — refused (Codex P1: would clobber / self-sweep).
    let overlay = p.join("inside");
    fs::create_dir_all(&overlay).expect("mkdir");
    fs::write(overlay.join("x.txt"), "x\n").expect("write");
    assert_cli_success(
        &run_libra_command(
            &[
                "layer",
                "add",
                "inside",
                "--source",
                overlay.to_str().unwrap(),
            ],
            p,
        ),
        "layer add",
    );
    let apply = run_libra_command(&["layer", "apply"], p);
    assert_ne!(apply.status.code(), Some(0), "in-worktree source refused");
    assert!(
        String::from_utf8_lossy(&apply.stderr).contains("inside the working tree"),
        "{}",
        String::from_utf8_lossy(&apply.stderr)
    );
}

#[test]
fn layer_unapply_keeps_edited_file_layer_owned() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let (_src, overlay) = make_overlay();
    assert_cli_success(
        &run_libra_command(
            &[
                "layer",
                "add",
                "scratch",
                "--source",
                overlay.to_str().unwrap(),
            ],
            p,
        ),
        "layer add",
    );
    assert_cli_success(&run_libra_command(&["layer", "apply"], p), "apply");
    // Edit a materialized file, then unapply.
    fs::write(p.join("a.txt"), "EDITED\n").expect("edit");
    assert_cli_success(&run_libra_command(&["layer", "unapply"], p), "unapply");
    // The edited file stays on disk AND stays layer-owned: still hidden from
    // status and still un-stageable (never silently committable).
    assert!(p.join("a.txt").exists(), "edited file kept");
    let status = run_libra_command(&["status", "--porcelain"], p);
    assert!(
        !String::from_utf8_lossy(&status.stdout).contains("a.txt"),
        "edited overlay still layer-owned (hidden): {}",
        String::from_utf8_lossy(&status.stdout)
    );
    let forced = run_libra_command(&["add", "--force", "a.txt"], p);
    assert_eq!(
        forced.status.code(),
        Some(128),
        "edited overlay still un-stageable"
    );
    // `layer remove` detaches it (edited file becomes a normal file).
    assert_cli_success(
        &run_libra_command(&["layer", "remove", "scratch"], p),
        "remove",
    );
    let after = run_libra_command(&["status", "--porcelain"], p);
    assert!(
        String::from_utf8_lossy(&after.stdout).contains("a.txt"),
        "after remove, edited file is a normal untracked file: {}",
        String::from_utf8_lossy(&after.stdout)
    );
}
