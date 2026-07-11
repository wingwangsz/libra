use super::*;

fn text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn diff_custom_prefixes_preserve_empty_and_boundary_whitespace_verbatim() {
    let fixture = Fixture::new();
    let repo = fixture.path("diff-prefix-verbatim-values");
    fixture.init_repo(&repo);
    fixture.commit_file(&repo, "tracked.txt", "old\n", "base");
    fs::write(repo.join("tracked.txt"), "new\n").expect("modify tracked file");
    fixture.success(&repo, &["config", "diff.srcPrefix", ""]);
    fixture.success(&repo, &["config", "diff.dstPrefix", " DST "]);

    let output = text(&fixture.success(&repo, &["diff"]));
    assert!(
        output
            .lines()
            .any(|line| line == "diff --git tracked.txt  DST tracked.txt"),
        "empty source and boundary-whitespace destination prefixes must remain verbatim: {output:?}"
    );
    assert!(output.contains("--- tracked.txt\n"), "{output:?}");
    assert!(output.contains("+++  DST tracked.txt\n"), "{output:?}");
}

#[test]
fn diff_custom_and_no_prefix_apply_to_unmerged_combined_headers() {
    let fixture = Fixture::new();
    let repo = fixture.path("diff-prefix-combined-conflict");
    fixture.init_repo(&repo);
    fixture.commit_file(&repo, "conflict.txt", "base\n", "base");

    fixture.success(&repo, &["switch", "-c", "side"]);
    fs::write(repo.join("conflict.txt"), "side\n").expect("write side change");
    fixture.success(&repo, &["add", "conflict.txt"]);
    fixture.success(
        &repo,
        &["commit", "--no-gpg-sign", "--no-verify", "-m", "side"],
    );

    fixture.success(&repo, &["switch", "main"]);
    fs::write(repo.join("conflict.txt"), "main\n").expect("write main change");
    fixture.success(&repo, &["add", "conflict.txt"]);
    fixture.success(
        &repo,
        &["commit", "--no-gpg-sign", "--no-verify", "-m", "main"],
    );
    let merge = fixture.run(&repo, &["merge", "side"]);
    assert!(!merge.status.success(), "merge must stop on the conflict");
    assert!(
        String::from_utf8_lossy(&merge.stderr).contains("LBR-CONFLICT-"),
        "merge must report the conflict: {}",
        String::from_utf8_lossy(&merge.stderr)
    );

    fixture.success(&repo, &["config", "diff.srcPrefix", "OLD/"]);
    fixture.success(&repo, &["config", "diff.dstPrefix", "NEW/"]);
    let custom = text(&fixture.success(&repo, &["diff"]));
    assert!(custom.contains("diff --cc conflict.txt"), "{custom}");
    assert!(custom.contains("--- OLD/conflict.txt"), "{custom}");
    assert!(custom.contains("+++ NEW/conflict.txt"), "{custom}");
    assert!(custom.contains("@@@ "), "{custom}");

    fixture.success(&repo, &["config", "diff.noPrefix", "true"]);
    let no_prefix = text(&fixture.success(&repo, &["diff"]));
    assert!(no_prefix.contains("diff --cc conflict.txt"), "{no_prefix}");
    assert!(no_prefix.contains("--- conflict.txt"), "{no_prefix}");
    assert!(no_prefix.contains("+++ conflict.txt"), "{no_prefix}");
    assert!(!no_prefix.contains("OLD/") && !no_prefix.contains("NEW/"));
}

#[test]
fn diff_custom_prefixes_preserve_dev_null_for_binary_add_delete_and_reverse() {
    let fixture = Fixture::new();
    let repo = fixture.path("diff-prefix-binary-dev-null");
    fixture.init_repo(&repo);
    fixture.success(&repo, &["config", "diff.srcPrefix", "OLD/"]);
    fixture.success(&repo, &["config", "diff.dstPrefix", "NEW/"]);

    fs::write(repo.join("binary.bin"), b"new\0binary\n").expect("write binary file");
    fixture.success(&repo, &["add", "binary.bin"]);
    let added = text(&fixture.success(&repo, &["diff", "--staged"]));
    assert!(
        added.contains("Binary files /dev/null and NEW/binary.bin differ"),
        "added binary keeps /dev/null and rewrites the destination: {added}"
    );
    let added_reverse = text(&fixture.success(&repo, &["diff", "--staged", "-R"]));
    assert!(
        added_reverse.contains("Binary files NEW/binary.bin and /dev/null differ"),
        "reversed added binary becomes a deletion with swapped prefixes: {added_reverse}"
    );

    fixture.success(
        &repo,
        &["commit", "--no-gpg-sign", "--no-verify", "-m", "binary"],
    );
    fs::remove_file(repo.join("binary.bin")).expect("remove binary file");
    fixture.success(&repo, &["add", "-A"]);
    let deleted = text(&fixture.success(&repo, &["diff", "--staged"]));
    assert!(
        deleted.contains("Binary files OLD/binary.bin and /dev/null differ"),
        "deleted binary rewrites the source and keeps /dev/null: {deleted}"
    );
    let deleted_reverse = text(&fixture.success(&repo, &["diff", "--staged", "-R"]));
    assert!(
        deleted_reverse.contains("Binary files /dev/null and OLD/binary.bin differ"),
        "reversed deletion becomes an addition with swapped prefixes: {deleted_reverse}"
    );
}

#[test]
fn diff_prefix_rewrite_never_touches_word_diff_content() {
    let fixture = Fixture::new();
    let repo = fixture.path("diff-prefix-word-body");
    fixture.init_repo(&repo);
    fixture.commit_file(
        &repo,
        "tracked.txt",
        "diff --git a/tracked.txt b/tracked.txt\n--- a/tracked.txt\n+++ b/tracked.txt\nBinary files a/tracked.txt and b/tracked.txt differ\nold\n",
        "base",
    );
    fs::write(
        repo.join("tracked.txt"),
        "diff --git a/tracked.txt b/tracked.txt\n--- a/tracked.txt\n+++ b/tracked.txt\nBinary files a/tracked.txt and b/tracked.txt differ\nnew\n",
    )
    .expect("modify collision fixture");
    fixture.success(&repo, &["config", "diff.srcPrefix", "OLD/"]);
    fixture.success(&repo, &["config", "diff.dstPrefix", "NEW/"]);

    let output = text(&fixture.success(&repo, &["diff", "--word-diff=plain", "-U4"]));
    assert!(
        output.contains("diff --git OLD/tracked.txt NEW/tracked.txt"),
        "metadata uses configured prefixes: {output}"
    );
    for literal in [
        "diff --git a/tracked.txt b/tracked.txt",
        "--- a/tracked.txt",
        "+++ b/tracked.txt",
        "Binary files a/tracked.txt and b/tracked.txt differ",
    ] {
        assert!(
            output.contains(literal),
            "word-diff content remains byte-for-byte literal '{literal}': {output}"
        );
    }
}

#[test]
fn diff_prefix_rewrite_preserves_crlf_hunk_bytes() {
    let fixture = Fixture::new();
    let repo = fixture.path("diff-prefix-crlf");
    fixture.init_repo(&repo);
    fixture.commit_file(&repo, "crlf.txt", "old\r\nkeep\r\n", "base");
    fs::write(repo.join("crlf.txt"), "new\r\nkeep\r\n").expect("modify CRLF file");
    let baseline = fixture.success(&repo, &["diff"]);
    fixture.success(&repo, &["config", "diff.srcPrefix", "OLD/"]);
    fixture.success(&repo, &["config", "diff.dstPrefix", "NEW/"]);

    let output = fixture.success(&repo, &["diff"]);
    let baseline_hunk = baseline
        .stdout
        .windows(3)
        .position(|window| window == b"@@ ")
        .expect("baseline hunk");
    let configured_hunk = output
        .stdout
        .windows(3)
        .position(|window| window == b"@@ ")
        .expect("configured hunk");
    assert!(
        baseline.stdout[baseline_hunk..] == output.stdout[configured_hunk..],
        "prefix rewriting must preserve the diff engine's hunk bytes exactly: baseline={:?}, configured={:?}",
        &baseline.stdout[baseline_hunk..],
        &output.stdout[configured_hunk..]
    );
}

#[test]
fn diff_custom_prefix_binary_patch_keeps_blank_terminator() {
    let fixture = Fixture::new();
    let repo = fixture.path("diff-prefix-binary-patch");
    fixture.init_repo(&repo);
    fixture.commit_file(&repo, "binary.bin", "old\0bytes\n", "base");
    fs::write(repo.join("binary.bin"), b"new\0bytes\n").expect("modify binary file");
    fixture.success(&repo, &["config", "diff.srcPrefix", "OLD/"]);
    fixture.success(&repo, &["config", "diff.dstPrefix", "NEW/"]);

    let output = fixture.success(&repo, &["diff", "--binary"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("diff --git OLD/binary.bin NEW/binary.bin"),
        "custom prefix must reach binary patch header: {stdout}"
    );
    assert!(
        output.stdout.ends_with(b"\n\n"),
        "binary patch must retain Git's blank-line terminator: {:?}",
        output.stdout
    );
}
