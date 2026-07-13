use super::*;

fn dirty_prefix_repo(fixture: &Fixture, name: &str) -> PathBuf {
    let repo = fixture.path(name);
    fixture.init_repo(&repo);
    fixture.commit_file(&repo, "tracked.txt", "old\n", "base");
    fs::write(repo.join("tracked.txt"), "new\n").expect("modify tracked file");
    repo
}

fn text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn assert_no_progress(output: &Output) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("Scanning working tree"), "{stderr}");
}

#[test]
fn diff_custom_prefixes_cascade_per_key() {
    let fixture = Fixture::new();
    let repo = dirty_prefix_repo(&fixture, "diff-prefix-cascade");

    fixture.success(&repo, &["config", "--system", "diff.srcPrefix", "SYS-S/"]);
    fixture.success(&repo, &["config", "--system", "diff.dstPrefix", "SYS-D/"]);
    fixture.success(
        &repo,
        &["config", "--global", "diff.srcPrefix", "GLOBAL-S/"],
    );
    fixture.success(&repo, &["config", "diff.dstPrefix", "LOCAL-D/"]);

    let output = text(&fixture.success(&repo, &["diff"]));
    assert!(
        output.contains("diff --git GLOBAL-S/tracked.txt LOCAL-D/tracked.txt"),
        "per-key cascade: {output}"
    );
    assert!(output.contains("--- GLOBAL-S/tracked.txt"), "{output}");
    assert!(output.contains("+++ LOCAL-D/tracked.txt"), "{output}");
}

#[test]
fn diff_mnemonic_and_noprefix_follow_git_precedence() {
    let fixture = Fixture::new();
    let repo = dirty_prefix_repo(&fixture, "diff-prefix-precedence");
    fixture.success(&repo, &["config", "diff.srcPrefix", "CUSTOM-S/"]);
    fixture.success(&repo, &["config", "diff.dstPrefix", "CUSTOM-D/"]);
    fixture.success(&repo, &["config", "diff.mnemonicPrefix", "true"]);

    let worktree = text(&fixture.success(&repo, &["diff"]));
    assert!(
        worktree.contains("diff --git i/tracked.txt w/tracked.txt"),
        "{worktree}"
    );
    let commit_worktree = text(&fixture.success(&repo, &["diff", "HEAD"]));
    assert!(
        commit_worktree.contains("diff --git c/tracked.txt w/tracked.txt"),
        "{commit_worktree}"
    );
    let reverse = text(&fixture.success(&repo, &["diff", "-R"]));
    assert!(
        reverse.contains("diff --git w/tracked.txt i/tracked.txt"),
        "{reverse}"
    );

    fixture.success(&repo, &["add", "tracked.txt"]);
    let staged = text(&fixture.success(&repo, &["diff", "--staged"]));
    assert!(
        staged.contains("diff --git c/tracked.txt i/tracked.txt"),
        "{staged}"
    );

    fixture.success(&repo, &["config", "diff.noPrefix", "true"]);
    let no_prefix = text(&fixture.success(&repo, &["diff", "--staged"]));
    assert!(
        no_prefix.contains("diff --git tracked.txt tracked.txt"),
        "{no_prefix}"
    );
    assert!(no_prefix.contains("--- tracked.txt"), "{no_prefix}");
    assert!(no_prefix.contains("+++ tracked.txt"), "{no_prefix}");
    assert!(!no_prefix.contains("CUSTOM-S/"), "{no_prefix}");
}

#[test]
fn diff_mnemonic_commit_pairs_and_plumbing_delegates() {
    let fixture = Fixture::new();
    let repo = fixture.path("diff-prefix-mnemonic-plumbing");
    fixture.init_repo(&repo);
    fixture.commit_file(&repo, "tracked.txt", "one\n", "one");
    fixture.commit_file(&repo, "tracked.txt", "two\n", "two");
    fixture.success(&repo, &["config", "diff.mnemonicPrefix", "true"]);

    let commits = text(&fixture.success(&repo, &["diff", "HEAD~1", "HEAD"]));
    assert!(
        commits.contains("diff --git c/tracked.txt c/tracked.txt"),
        "commit/commit mnemonic pair: {commits}"
    );

    let tree = fixture.run(&repo, &["diff-tree", "HEAD~1", "HEAD"]);
    assert_eq!(tree.status.code(), Some(1));
    assert!(
        text(&tree).contains("diff --git c/tracked.txt c/tracked.txt"),
        "diff-tree delegates commit/commit prefixes: {}",
        text(&tree)
    );

    let index = fixture.run(&repo, &["diff-index", "HEAD~1"]);
    assert_eq!(index.status.code(), Some(1));
    assert!(
        text(&index).contains("diff --git c/tracked.txt w/tracked.txt"),
        "diff-index delegates commit/worktree prefixes: {}",
        text(&index)
    );

    fs::write(repo.join("tracked.txt"), "three\n").expect("modify worktree");
    let files = fixture.run(&repo, &["diff-files"]);
    assert_eq!(files.status.code(), Some(1));
    assert!(
        text(&files).contains("diff --git i/tracked.txt w/tracked.txt"),
        "diff-files delegates index/worktree prefixes: {}",
        text(&files)
    );
}

#[test]
fn diff_prefix_boolean_errors_fail_before_progress() {
    let fixture = Fixture::new();
    let repo = dirty_prefix_repo(&fixture, "diff-prefix-errors");

    for (key, value) in [
        ("diff.noPrefix", "sideways"),
        ("diff.mnemonicPrefix", "maybe"),
    ] {
        fixture.success(&repo, &["config", key, value]);
        let rejected = fixture.run(&repo, &["diff"]);
        assert_eq!(rejected.status.code(), Some(129));
        let stderr = String::from_utf8_lossy(&rejected.stderr);
        assert!(stderr.contains("LBR-CLI-002"), "{stderr}");
        assert!(stderr.contains(key), "{stderr}");
        assert!(rejected.stdout.is_empty());
        assert_no_progress(&rejected);
        fixture.success(&repo, &["config", "--unset", key]);
    }
}

#[test]
fn diff_prefix_read_failure_names_the_key_before_progress() {
    let fixture = Fixture::new();
    let repo = dirty_prefix_repo(&fixture, "diff-prefix-read-error");
    for (key, value) in [
        ("diff.context", "3"),
        ("diff.renames", "true"),
        ("diff.noPrefix", "false"),
        ("diff.mnemonicPrefix", "false"),
    ] {
        fixture.success(&repo, &["config", key, value]);
    }
    fs::create_dir_all(fixture.home.join(".libra").join("config.db"))
        .expect("create unreadable global config path");
    let rejected = fixture.run(&repo, &["diff"]);
    assert_eq!(rejected.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(stderr.contains("LBR-IO-001"), "{stderr}");
    assert!(stderr.contains("diff.srcPrefix"), "{stderr}");
    assert!(rejected.stdout.is_empty());
    assert_no_progress(&rejected);
}

#[test]
fn diff_prefixes_apply_after_relative_and_to_rename_bodies() {
    let fixture = Fixture::new();
    let repo = fixture.path("diff-prefix-rename-relative");
    fixture.init_repo(&repo);
    fs::create_dir_all(repo.join("sub")).expect("create fixture subdirectory");
    fixture.commit_file(
        &repo,
        "sub/old name.txt",
        "alpha\nbeta\ngamma\ndelta\nepsilon\n",
        "base",
    );
    fs::rename(repo.join("sub/old name.txt"), repo.join("sub/new name.txt"))
        .expect("rename fixture file");
    fs::write(
        repo.join("sub/new name.txt"),
        "alpha\nbeta\nGAMMA\ndelta\nepsilon\n",
    )
    .expect("modify renamed fixture file");
    fixture.success(&repo, &["add", "-A"]);
    fixture.success(&repo, &["config", "diff.srcPrefix", "OLD/"]);
    fixture.success(&repo, &["config", "diff.dstPrefix", "NEW/"]);

    let output = text(&fixture.success(&repo, &["diff", "--staged", "--relative=sub"]));
    assert!(
        output.contains("diff --git OLD/old name.txt NEW/new name.txt"),
        "{output}"
    );
    assert!(output.contains("rename from old name.txt"), "{output}");
    assert!(output.contains("rename to new name.txt"), "{output}");
    assert!(output.contains("--- OLD/old name.txt"), "{output}");
    assert!(output.contains("+++ NEW/new name.txt"), "{output}");
    assert!(
        !output.contains("OLD/sub/"),
        "relative runs first: {output}"
    );
}
