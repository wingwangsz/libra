//! Apply plain-text `format-patch` mail messages as commits.
//!
//! This is the deliberately small P2 mail-flow surface: one or more patch
//! files plus `--continue`, `--skip`, and `--abort`. The parser accepts the
//! common 8-bit, quoted-printable, and base64 single-part mail forms. MIME
//! multipart messages and the wider Git `am` option set remain out of scope.

use std::{collections::HashSet, fs, str::FromStr};

use base64::Engine as _;
use chrono::DateTime;
use clap::Parser;
use git_internal::{
    hash::ObjectHash,
    internal::{index::Index, object::commit::Commit},
};
use serde::{Deserialize, Serialize};

use crate::{
    command::{
        add::{AddArgs, run_add},
        apply::{
            MAX_PATCH_BYTES, PatchPreparationError, patch_targets, prepare_patch,
            validate_patch_target,
        },
        commit::create_commit_signatures,
        save_object, status,
    },
    common_utils::format_commit_msg,
    internal::{
        branch::Branch,
        head::Head,
        reflog::{ReflogAction, ReflogContext, with_reflog},
        sequencer::{self, AmSequenceState},
        tree_plumbing,
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        path,
        text::short_display_hash,
        util,
    },
};

const MAX_MAILS: usize = 10_000;

pub const AM_EXAMPLES: &str = "\
EXAMPLES:
    libra am 0001-fix.patch              Apply one format-patch mail
    libra am 0001.patch 0002.patch       Apply a mail series in order
    libra am --continue                  Commit a staged conflict resolution
    libra am --skip                      Skip the current mail
    libra am --abort                     Restore the pre-am branch tip";

#[derive(Parser, Debug)]
#[command(after_help = AM_EXAMPLES)]
pub struct AmArgs {
    /// Plain-text format-patch mail files, applied in order.
    #[clap(
        value_name = "PATCH",
        required_unless_present_any = ["continue_am", "skip", "abort"]
    )]
    pub patches: Vec<String>,

    /// Commit the staged resolution and resume the mail series.
    #[clap(
        long = "continue",
        conflicts_with_all = ["patches", "skip", "abort"]
    )]
    pub continue_am: bool,

    /// Discard the current mail's changes and resume with the next mail.
    #[clap(
        long,
        conflicts_with_all = ["patches", "continue_am", "abort"]
    )]
    pub skip: bool,

    /// Restore the original branch tip, index, and tracked worktree.
    #[clap(
        long,
        conflicts_with_all = ["patches", "continue_am", "skip"]
    )]
    pub abort: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct MailPatch {
    source: String,
    author: String,
    author_date: String,
    message: String,
    patch: String,
    targets: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct AmPayload {
    patches: Vec<MailPatch>,
    current: usize,
}

#[derive(Clone, Debug)]
struct AmState {
    head_name: String,
    head_orig: ObjectHash,
    expected_head: ObjectHash,
    payload: AmPayload,
}

impl AmState {
    fn to_sequence(&self) -> CliResult<AmSequenceState> {
        let payload = serde_json::to_string(&self.payload)
            .map_err(|error| am_state_error(format!("failed to serialize am state: {error}")))?;
        Ok(AmSequenceState {
            head_name: self.head_name.clone(),
            head_orig: self.head_orig.to_string(),
            current_oid: self.expected_head.to_string(),
            todo: self.payload.patches[self.payload.current.saturating_add(1)..]
                .iter()
                .map(|mail| mail.source.clone())
                .collect(),
            payload,
        })
    }

    fn from_sequence(sequence: AmSequenceState) -> CliResult<Self> {
        let head_orig = ObjectHash::from_str(sequence.head_orig.trim()).map_err(|error| {
            am_state_error(format!("saved am original HEAD is invalid: {error}"))
        })?;
        let expected_head = ObjectHash::from_str(sequence.current_oid.trim()).map_err(|error| {
            am_state_error(format!("saved am expected HEAD is invalid: {error}"))
        })?;
        let payload: AmPayload = serde_json::from_str(&sequence.payload)
            .map_err(|error| am_state_error(format!("saved am state is invalid: {error}")))?;
        if payload.patches.is_empty()
            || payload.patches.len() > MAX_MAILS
            || payload.current >= payload.patches.len()
        {
            return Err(am_state_error(
                "saved am state has an invalid patch position".to_string(),
            ));
        }
        let mut total = 0usize;
        for mail in &payload.patches {
            total = total
                .checked_add(mail.patch.len())
                .ok_or_else(|| am_state_error("saved am patch size overflow".to_string()))?;
            if total > MAX_PATCH_BYTES
                || validate_author(&mail.author).is_err()
                || mail.message.is_empty()
                || mail.message.contains('\0')
            {
                return Err(am_state_error(
                    "saved am state contains invalid mail metadata".to_string(),
                ));
            }
            let targets = patch_targets(&mail.patch, 1)
                .map_err(|error| am_state_error(format!("saved am patch is invalid: {error}")))?;
            if targets.is_empty() || targets != mail.targets {
                return Err(am_state_error(
                    "saved am patch target list does not match its patch".to_string(),
                ));
            }
        }
        Ok(Self {
            head_name: sequence.head_name,
            head_orig,
            expected_head,
            payload,
        })
    }

    async fn save(&self) -> CliResult<()> {
        let sequence = self.to_sequence()?;
        sequencer::save_am(&sequence)
            .await
            .map_err(|error| am_state_error(format!("failed to save am state: {error}")))
    }

    async fn load() -> CliResult<Option<Self>> {
        match sequencer::load_am()
            .await
            .map_err(|error| am_state_error(format!("failed to load am state: {error}")))?
        {
            Some(sequence) => Self::from_sequence(sequence).map(Some),
            None => Ok(None),
        }
    }
}

#[derive(Debug, Serialize)]
struct AppliedMail {
    source: String,
    subject: String,
    commit: String,
}

#[derive(Debug, Serialize)]
struct AmOutput {
    action: String,
    applied: Vec<AppliedMail>,
    #[serde(skip_serializing_if = "Option::is_none")]
    restored_head: Option<String>,
}

pub async fn execute_safe(args: AmArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;
    crate::command::ensure_main_worktree("am")?;

    let result = if args.continue_am {
        continue_am(output).await?
    } else if args.skip {
        skip_am(output).await?
    } else if args.abort {
        abort_am(output).await?
    } else {
        sequencer::ensure_none_for_am().await?;
        if AmState::load().await?.is_some() {
            return Err(CliError::conflict("an am operation is already in progress")
                .with_hint("use 'libra am --continue', '--skip', or '--abort'"));
        }
        start_am(&args.patches, output).await?
    };

    render_output(&result, output)
}

async fn start_am(paths: &[String], output: &OutputConfig) -> CliResult<AmOutput> {
    let patches = read_mail_patches(paths)?;
    ensure_clean_start(&patches).await?;
    let head_name = current_branch_name().await?;
    let head_orig = Head::current_commit_result()
        .await
        .map_err(|error| am_state_error(format!("failed to resolve HEAD commit: {error}")))?
        .ok_or_else(|| {
            CliError::fatal("am requires an existing HEAD commit")
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("create an initial commit before applying a mail series")
        })?;
    let mut state = AmState {
        head_name,
        head_orig,
        expected_head: head_orig,
        payload: AmPayload {
            patches,
            current: 0,
        },
    };
    state.save().await?;
    if test_failpoint_enabled("LIBRA_TEST_AM_FAIL_AFTER_STATE") {
        return Err(am_state_error(
            "test-injected am interruption after saving initial state".to_string(),
        )
        .with_hint("run 'libra am --continue' to resume the mail series")
        .with_hint("or run 'libra am --abort' to restore the original branch"));
    }
    let applied = apply_remaining(&mut state, output).await?;
    Ok(AmOutput {
        action: "apply".to_string(),
        applied,
        restored_head: None,
    })
}

async fn continue_am(output: &OutputConfig) -> CliResult<AmOutput> {
    let mut state = load_state_or_error().await?;
    ensure_state_branch(&state).await?;
    ensure_expected_head(&state).await?;
    let mail = state.payload.patches[state.payload.current].clone();
    if recovery_state_is_pristine(&mail.targets).await? {
        let applied = apply_remaining(&mut state, output).await?;
        return Ok(AmOutput {
            action: "continue".to_string(),
            applied,
            restored_head: None,
        });
    }
    ensure_resolution_staged(&mail.targets).await?;

    let first = commit_current(&mut state, mail).await?;
    let mut applied = vec![first];
    applied.extend(apply_remaining(&mut state, output).await?);
    Ok(AmOutput {
        action: "continue".to_string(),
        applied,
        restored_head: None,
    })
}

async fn skip_am(output: &OutputConfig) -> CliResult<AmOutput> {
    let mut state = load_state_or_error().await?;
    ensure_state_branch(&state).await?;
    ensure_expected_head(&state).await?;
    let current_targets = state.payload.patches[state.payload.current].targets.clone();
    reset_hard("HEAD", output).await?;
    cleanup_untracked_patch_targets(&current_targets)?;

    state.payload.current += 1;
    if state.payload.current == state.payload.patches.len() {
        sequencer::clear_am()
            .await
            .map_err(|error| am_state_error(format!("failed to clear am state: {error}")))?;
        return Ok(AmOutput {
            action: "skip".to_string(),
            applied: Vec::new(),
            restored_head: None,
        });
    }
    state.save().await?;
    let applied = apply_remaining(&mut state, output).await?;
    Ok(AmOutput {
        action: "skip".to_string(),
        applied,
        restored_head: None,
    })
}

async fn abort_am(output: &OutputConfig) -> CliResult<AmOutput> {
    let state = load_state_or_error().await?;
    ensure_state_branch(&state).await?;
    let current_targets = state.payload.patches[state.payload.current].targets.clone();
    let restored = state.head_orig.to_string();
    reset_hard(&restored, output).await?;
    cleanup_untracked_patch_targets(&current_targets)?;
    sequencer::clear_am()
        .await
        .map_err(|error| am_state_error(format!("failed to clear am state: {error}")))?;
    Ok(AmOutput {
        action: "abort".to_string(),
        applied: Vec::new(),
        restored_head: Some(restored),
    })
}

async fn apply_remaining(
    state: &mut AmState,
    _output: &OutputConfig,
) -> CliResult<Vec<AppliedMail>> {
    let mut applied = Vec::new();
    while state.payload.current < state.payload.patches.len() {
        ensure_expected_head(state).await?;
        let mail = state.payload.patches[state.payload.current].clone();
        let prepared = match prepare_patch(&mail.patch, 1, &util::working_dir()) {
            Ok(prepared) => prepared,
            Err(PatchPreparationError::DoesNotApply(detail)) => {
                return Err(am_conflict(&mail, detail));
            }
            Err(PatchPreparationError::Invalid(detail)) => {
                return Err(
                    am_state_error(format!("cannot apply '{}': {detail}", mail.source))
                        .with_hint("run 'libra am --abort' to restore the original branch"),
                );
            }
        };
        prepared.write().map_err(|detail| {
            am_state_error(format!("cannot apply '{}': {detail}", mail.source))
                .with_hint("fix and stage the affected paths, then run 'libra am --continue'")
                .with_hint("or run 'libra am --abort' to restore the original branch")
        })?;
        if test_failpoint_enabled("LIBRA_TEST_AM_FAIL_AFTER_WRITE") {
            return Err(am_state_error(
                "test-injected am interruption after worktree write".to_string(),
            )
            .with_hint("run 'libra am --abort' to restore the original branch"));
        }
        stage_targets(&mail.targets).await?;
        ensure_resolution_staged(&mail.targets).await?;
        applied.push(commit_current(state, mail).await?);
        if state.payload.current < state.payload.patches.len()
            && test_failpoint_enabled("LIBRA_TEST_AM_FAIL_AFTER_COMMIT")
        {
            return Err(am_state_error(
                "test-injected am interruption between commits".to_string(),
            )
            .with_hint("run 'libra am --continue' to resume the mail series")
            .with_hint("or run 'libra am --abort' to restore the original branch"));
        }
    }
    Ok(applied)
}

async fn commit_current(state: &mut AmState, mail: MailPatch) -> CliResult<AppliedMail> {
    let index = Index::load(path::index())
        .map_err(|error| am_state_error(format!("failed to load the index for am: {error}")))?;
    let tree_id = tree_plumbing::write_tree_from_index(&index)
        .map_err(|error| am_state_error(format!("failed to create am commit tree: {error}")))?;
    let parent = Head::current_commit_result()
        .await
        .map_err(|error| am_state_error(format!("failed to resolve HEAD commit: {error}")))?
        .ok_or_else(|| am_state_error("HEAD disappeared during am".to_string()))?;
    if parent != state.expected_head {
        return Err(am_head_moved(state, parent));
    }
    let (author, committer, _) =
        create_commit_signatures(Some(mail.author.as_str()), Some(mail.author_date.as_str()))
            .await
            .map_err(CliError::from)?;
    let commit = Commit::new(
        author,
        committer,
        tree_id,
        vec![parent],
        &format_commit_msg(&mail.message, None),
    );
    save_object(&commit, &commit.id).map_err(|error| {
        am_state_error(format!(
            "failed to save commit for '{}': {error}",
            mail.source
        ))
    })?;

    let mut next = state.clone();
    next.payload.current += 1;
    next.expected_head = commit.id;
    let next_sequence = if next.payload.current < next.payload.patches.len() {
        Some(next.to_sequence()?)
    } else {
        None
    };
    let branch = state.head_name.clone();
    let new_id = commit.id.to_string();
    let transaction_id = new_id.clone();
    let context = ReflogContext {
        old_oid: parent.to_string(),
        new_oid: new_id.clone(),
        action: ReflogAction::Commit {
            message: mail.message.clone(),
        },
    };
    with_reflog(
        context,
        move |txn| {
            Box::pin(async move {
                Branch::update_branch_with_conn(txn, &branch, &transaction_id, None).await?;
                match next_sequence {
                    Some(sequence) => sequencer::save_am_with_conn(txn, &sequence).await?,
                    None => sequencer::clear_am_with_conn(txn).await?,
                }
                Ok(())
            })
        },
        true,
    )
    .await
    .map_err(|error| {
        am_state_error(format!(
            "failed to update the branch, reflog, and am state for '{}': {error}",
            mail.source
        ))
    })?;
    *state = next;

    Ok(AppliedMail {
        source: mail.source,
        subject: mail_subject(&mail.message),
        commit: new_id,
    })
}

async fn ensure_clean_start(patches: &[MailPatch]) -> CliResult<()> {
    let staged = status::changes_to_be_committed_safe()
        .await
        .map_err(|error| am_state_error(format!("failed to inspect staged changes: {error}")))?;
    let unstaged = status::changes_to_be_staged().map_err(|error| {
        am_state_error(format!("failed to inspect working tree changes: {error}"))
    })?;
    if !staged.is_empty() || !unstaged.modified.is_empty() || !unstaged.deleted.is_empty() {
        return Err(CliError::conflict(
            "cannot start am with staged or tracked working-tree changes",
        )
        .with_hint("commit, stash, or restore the changes before running 'libra am'"));
    }
    let index = Index::load(path::index())
        .map_err(|error| am_state_error(format!("failed to load the index for am: {error}")))?;
    let workdir = util::working_dir();
    let mut checked = HashSet::new();
    for target in patches.iter().flat_map(|mail| &mail.targets) {
        if !checked.insert(target.as_str()) || index.tracked(target, 0) {
            continue;
        }
        let absolute = validate_patch_target(target, &workdir).map_err(|error| {
            am_state_error(format!("unsafe mail patch target '{target}': {error}"))
        })?;
        match fs::symlink_metadata(&absolute) {
            Ok(_) => {
                return Err(CliError::conflict(format!(
                    "untracked working-tree path '{target}' would be overwritten by am"
                ))
                .with_hint("move or remove the untracked path before running 'libra am'"));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(am_state_error(format!(
                    "failed to inspect untracked patch target '{target}': {error}"
                )));
            }
        }
    }
    Ok(())
}

async fn ensure_resolution_staged(targets: &[String]) -> CliResult<()> {
    let expected: HashSet<&str> = targets.iter().map(String::as_str).collect();
    let index = Index::load(path::index()).map_err(|error| {
        am_state_error(format!(
            "failed to load the index for am --continue: {error}"
        ))
    })?;
    let unresolved = crate::command::merge::unresolved_conflicted_paths(&index, &[]);
    if !unresolved.is_empty() {
        return Err(am_resolution_error(
            "the current am patch still has unresolved index entries",
        ));
    }

    let unstaged = status::changes_to_be_staged().map_err(|error| {
        am_state_error(format!("failed to inspect working tree changes: {error}"))
    })?;
    let has_target_unstaged = unstaged
        .polymerization()
        .iter()
        .any(|path| expected.contains(path.to_string_lossy().as_ref()));
    if has_target_unstaged {
        return Err(am_resolution_error(
            "the current am patch has changes that are not staged",
        ));
    }
    let unexpected_tracked = unstaged
        .modified
        .iter()
        .chain(&unstaged.deleted)
        .find(|path| !expected.contains(path.to_string_lossy().as_ref()))
        .cloned()
        .or_else(|| {
            unstaged
                .renamed
                .iter()
                .find(|(old, new)| {
                    !expected.contains(old.to_string_lossy().as_ref())
                        || !expected.contains(new.to_string_lossy().as_ref())
                })
                .map(|(_, new)| new.clone())
        });
    if let Some(path) = unexpected_tracked {
        return Err(am_resolution_error(&format!(
            "tracked path '{}' outside the current am patch has unstaged changes",
            path.display()
        )));
    }

    let staged = status::changes_to_be_committed_safe()
        .await
        .map_err(|error| am_state_error(format!("failed to inspect staged changes: {error}")))?;
    if staged.is_empty() {
        return Err(am_resolution_error(
            "the current am patch has no staged resolution",
        ));
    }
    let unexpected = staged
        .polymerization()
        .into_iter()
        .find(|path| !expected.contains(path.to_string_lossy().as_ref()));
    if let Some(path) = unexpected {
        return Err(am_resolution_error(&format!(
            "staged path '{}' is outside the current am patch",
            path.display()
        )));
    }
    Ok(())
}

async fn recovery_state_is_pristine(targets: &[String]) -> CliResult<bool> {
    let index = Index::load(path::index()).map_err(|error| {
        am_state_error(format!(
            "failed to load the index for am --continue: {error}"
        ))
    })?;
    if !crate::command::merge::unresolved_conflicted_paths(&index, &[]).is_empty() {
        return Ok(false);
    }
    let staged = status::changes_to_be_committed_safe()
        .await
        .map_err(|error| am_state_error(format!("failed to inspect staged changes: {error}")))?;
    if !staged.is_empty() {
        return Ok(false);
    }
    let unstaged = status::changes_to_be_staged().map_err(|error| {
        am_state_error(format!("failed to inspect working tree changes: {error}"))
    })?;
    if !unstaged.modified.is_empty() || !unstaged.deleted.is_empty() || !unstaged.renamed.is_empty()
    {
        return Ok(false);
    }
    let expected: HashSet<&str> = targets.iter().map(String::as_str).collect();
    Ok(!unstaged
        .new
        .iter()
        .any(|path| expected.contains(path.to_string_lossy().as_ref())))
}

fn test_failpoint_enabled(name: &str) -> bool {
    std::env::var_os("LIBRA_TEST").is_some() && std::env::var_os(name).is_some()
}

async fn stage_targets(targets: &[String]) -> CliResult<()> {
    let args = AddArgs {
        pathspec: targets.to_vec(),
        all: false,
        update: false,
        refresh: false,
        verbose: false,
        force: true,
        dry_run: false,
        ignore_errors: false,
        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    };
    run_add(&args).await.map(|_| ()).map_err(|error| {
        am_state_error(format!(
            "failed to stage the applied mail patch: {}",
            error.message()
        ))
        .with_hint("fix and stage the affected paths, then run 'libra am --continue'")
        .with_hint("or run 'libra am --abort' to restore the original branch")
    })
}

fn read_mail_patches(paths: &[String]) -> CliResult<Vec<MailPatch>> {
    if paths.len() > MAX_MAILS {
        return Err(CliError::command_usage(format!(
            "am accepts at most {MAX_MAILS} patch files"
        )));
    }
    let mut total = 0usize;
    let mut patches = Vec::with_capacity(paths.len());
    for source in paths {
        let bytes = fs::read(source).map_err(|error| {
            CliError::fatal(format!("cannot read mail patch '{source}': {error}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
        total = total.checked_add(bytes.len()).ok_or_else(|| {
            CliError::fatal("mail patch input size overflow")
                .with_stable_code(StableErrorCode::CliInvalidArguments)
        })?;
        if total > MAX_PATCH_BYTES {
            return Err(CliError::fatal(format!(
                "mail patch series exceeds the {} MiB limit",
                MAX_PATCH_BYTES / (1024 * 1024)
            ))
            .with_stable_code(StableErrorCode::CliInvalidArguments));
        }
        let text = String::from_utf8(bytes).map_err(|_| {
            CliError::fatal(format!("mail patch '{source}' is not valid UTF-8"))
                .with_stable_code(StableErrorCode::CliInvalidArguments)
        })?;
        patches.push(parse_mail_patch(source, &text)?);
    }
    Ok(patches)
}

fn parse_mail_patch(source: &str, raw: &str) -> CliResult<MailPatch> {
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let without_envelope = normalized
        .strip_prefix("From ")
        .and_then(|text| text.split_once('\n').map(|(_, rest)| rest))
        .unwrap_or(&normalized);
    let (raw_headers, encoded_body) = without_envelope
        .split_once("\n\n")
        .ok_or_else(|| invalid_mail(source, "missing blank line after mail headers"))?;
    let headers = parse_headers(raw_headers).map_err(|detail| invalid_mail(source, &detail))?;
    let content_type = header(&headers, "content-type").unwrap_or("text/plain");
    validate_content_type(content_type).map_err(|detail| invalid_mail(source, &detail))?;
    let transfer = header(&headers, "content-transfer-encoding").unwrap_or("8bit");
    let body =
        decode_transfer(encoded_body, transfer).map_err(|detail| invalid_mail(source, &detail))?;
    if body.contains('\0') {
        return Err(invalid_mail(source, "decoded mail body contains NUL"));
    }

    let mut author = decode_encoded_words(required_header(&headers, "from", source)?)
        .map_err(|detail| invalid_mail(source, &detail))?;
    let date = required_header(&headers, "date", source)?;
    let author_date =
        normalize_author_date(date).map_err(|detail| invalid_mail(source, &detail))?;
    let subject = decode_encoded_words(required_header(&headers, "subject", source)?)
        .map_err(|detail| invalid_mail(source, &detail))?;
    validate_decoded_header("Subject", &subject).map_err(|detail| invalid_mail(source, &detail))?;
    let subject = clean_patch_subject(&subject);
    if subject.is_empty() {
        return Err(invalid_mail(source, "patch subject is empty"));
    }

    let diff_start = body
        .lines()
        .enumerate()
        .find_map(|(index, line)| line.starts_with("diff --git ").then_some(index))
        .ok_or_else(|| invalid_mail(source, "mail body contains no 'diff --git' patch"))?;
    let lines: Vec<&str> = body.lines().collect();
    let separator = lines[..diff_start]
        .iter()
        .rposition(|line| *line == "---")
        .ok_or_else(|| invalid_mail(source, "mail patch is missing the '---' separator"))?;
    let mut message_lines = lines[..separator].to_vec();
    while message_lines.first().is_some_and(|line| line.is_empty()) {
        message_lines.remove(0);
    }
    if let Some(in_body_from) = message_lines
        .first()
        .and_then(|line| line.strip_prefix("From: "))
    {
        author = in_body_from.trim().to_string();
        message_lines.remove(0);
        if message_lines.first().is_some_and(|line| line.is_empty()) {
            message_lines.remove(0);
        }
    }
    validate_author(&author).map_err(|detail| invalid_mail(source, &detail))?;
    let body_message = message_lines.join("\n").trim().to_string();
    let message = if body_message.is_empty() {
        subject
    } else {
        format!("{subject}\n\n{body_message}")
    };

    let mut patch = format!("{}\n", lines[diff_start..].join("\n"));
    if let Some(signature) = patch.find("\n-- \n") {
        patch.truncate(signature + 1);
    }
    let targets = patch_targets(&patch, 1).map_err(|detail| invalid_mail(source, &detail))?;
    if targets.is_empty() {
        return Err(invalid_mail(source, "mail patch contains no file changes"));
    }
    Ok(MailPatch {
        source: source.to_string(),
        author,
        author_date,
        message,
        patch,
        targets,
    })
}

fn parse_headers(raw: &str) -> Result<Vec<(String, String)>, String> {
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in raw.lines() {
        if line.starts_with([' ', '\t']) {
            let (_, value) = headers
                .last_mut()
                .ok_or_else(|| "mail header continuation has no preceding header".to_string())?;
            value.push(' ');
            value.push_str(line.trim());
            continue;
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| format!("malformed mail header '{line}'"))?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(format!("invalid mail header name '{name}'"));
        }
        headers.push((name.to_ascii_lowercase(), value.trim().to_string()));
    }
    Ok(headers)
}

fn validate_content_type(value: &str) -> Result<(), String> {
    let mut parts = value.split(';');
    let media_type = parts.next().unwrap_or_default().trim();
    if !media_type.eq_ignore_ascii_case("text/plain") {
        return Err(format!(
            "unsupported Content-Type '{media_type}'; expected text/plain"
        ));
    }
    for parameter in parts {
        let Some((name, value)) = parameter.trim().split_once('=') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("charset") {
            let charset = value.trim().trim_matches('"');
            if !matches!(charset.to_ascii_lowercase().as_str(), "utf-8" | "us-ascii") {
                return Err(format!("unsupported text/plain charset '{charset}'"));
            }
        }
    }
    Ok(())
}

fn validate_decoded_header(name: &str, value: &str) -> Result<(), String> {
    if value.chars().any(char::is_control) {
        return Err(format!(
            "decoded {name} header contains a control character"
        ));
    }
    Ok(())
}

fn validate_author(author: &str) -> Result<(), String> {
    validate_decoded_header("From", author)?;
    let author = author.trim();
    let Some(start) = author.find('<') else {
        return Err("From header must use 'Name <email>' format".to_string());
    };
    let Some(relative_end) = author[start..].find('>') else {
        return Err("From header must use 'Name <email>' format".to_string());
    };
    let end = start + relative_end;
    let name = author[..start].trim();
    let email = author[start + 1..end].trim();
    if name.is_empty()
        || email.is_empty()
        || end != author.len() - 1
        || name.contains(['<', '>'])
        || email.contains(['<', '>'])
    {
        return Err("From header must use 'Name <email>' format".to_string());
    }
    Ok(())
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(candidate, _)| candidate == name)
        .map(|(_, value)| value.as_str())
}

fn required_header<'a>(
    headers: &'a [(String, String)],
    name: &str,
    source: &str,
) -> CliResult<&'a str> {
    header(headers, name)
        .ok_or_else(|| invalid_mail(source, &format!("missing required {name} header")))
}

fn decode_transfer(body: &str, encoding: &str) -> Result<String, String> {
    match encoding.trim().to_ascii_lowercase().as_str() {
        "" | "7bit" | "8bit" | "binary" => Ok(body.to_string()),
        "base64" => {
            let compact: String = body
                .chars()
                .filter(|ch| !ch.is_ascii_whitespace())
                .collect();
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(compact)
                .map_err(|error| format!("invalid base64 mail body: {error}"))?;
            String::from_utf8(bytes).map_err(|_| "decoded mail body is not UTF-8".to_string())
        }
        "quoted-printable" => decode_quoted_printable(body),
        other => Err(format!("unsupported Content-Transfer-Encoding '{other}'")),
    }
}

fn decode_quoted_printable(input: &str) -> Result<String, String> {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'=' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        if bytes.get(index + 1) == Some(&b'\n') {
            index += 2;
            continue;
        }
        let high = bytes
            .get(index + 1)
            .and_then(|byte| hex_value(*byte))
            .ok_or_else(|| "invalid quoted-printable escape".to_string())?;
        let low = bytes
            .get(index + 2)
            .and_then(|byte| hex_value(*byte))
            .ok_or_else(|| "invalid quoted-printable escape".to_string())?;
        decoded.push((high << 4) | low);
        index += 3;
    }
    String::from_utf8(decoded).map_err(|_| "decoded mail body is not UTF-8".to_string())
}

fn decode_encoded_words(input: &str) -> Result<String, String> {
    let mut output = String::new();
    let mut rest = input;
    while let Some(start) = rest.find("=?") {
        output.push_str(&rest[..start]);
        let word = &rest[start + 2..];
        let (charset, after_charset) = word
            .split_once('?')
            .ok_or_else(|| "malformed RFC 2047 encoded word".to_string())?;
        let (encoding, after_encoding) = after_charset
            .split_once('?')
            .ok_or_else(|| "malformed RFC 2047 encoded word".to_string())?;
        let (encoded, after_word) = after_encoding
            .split_once("?=")
            .ok_or_else(|| "malformed RFC 2047 encoded word".to_string())?;
        if !matches!(charset.to_ascii_lowercase().as_str(), "utf-8" | "us-ascii") {
            return Err(format!("unsupported RFC 2047 charset '{charset}'"));
        }
        let decoded = match encoding.to_ascii_lowercase().as_str() {
            "b" => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .map_err(|error| format!("invalid RFC 2047 base64 word: {error}"))?;
                String::from_utf8(bytes)
                    .map_err(|_| "decoded RFC 2047 word is not UTF-8".to_string())?
            }
            "q" => decode_quoted_printable(&encoded.replace('_', " "))?,
            other => return Err(format!("unsupported RFC 2047 encoding '{other}'")),
        };
        output.push_str(&decoded);
        rest = after_word;
    }
    output.push_str(rest);
    Ok(output)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn clean_patch_subject(subject: &str) -> String {
    let trimmed = subject.trim();
    if let Some(close) = trimmed.find(']')
        && trimmed.starts_with('[')
        && matches!(
            trimmed[1..close]
                .trim()
                .split_ascii_whitespace()
                .next(),
            Some(marker) if marker.eq_ignore_ascii_case("patch")
        )
    {
        return trimmed[close + 1..].trim().to_string();
    }
    trimmed.to_string()
}

fn normalize_author_date(value: &str) -> Result<String, String> {
    let date = DateTime::parse_from_rfc2822(value)
        .map_err(|error| format!("invalid Date header '{value}': {error}"))?;
    let seconds = date.offset().local_minus_utc();
    let sign = if seconds < 0 { '-' } else { '+' };
    let absolute = seconds.unsigned_abs();
    Ok(format!(
        "{} {sign}{:02}{:02}",
        date.timestamp(),
        absolute / 3600,
        (absolute % 3600) / 60
    ))
}

async fn load_state_or_error() -> CliResult<AmState> {
    AmState::load().await?.ok_or_else(|| {
        CliError::conflict("no am operation is in progress")
            .with_hint("start one with 'libra am <patch>...' ")
    })
}

async fn current_branch_name() -> CliResult<String> {
    match Head::current_result()
        .await
        .map_err(|error| am_state_error(format!("failed to resolve HEAD: {error}")))?
    {
        Head::Branch(name) => Ok(name),
        Head::Detached(_) => Err(CliError::conflict("am cannot run on a detached HEAD")
            .with_hint("switch to a local branch before running 'libra am'")),
    }
}

async fn ensure_state_branch(state: &AmState) -> CliResult<()> {
    let current = current_branch_name().await?;
    if current != state.head_name {
        return Err(CliError::conflict(format!(
            "am started on branch '{}' but HEAD is now on '{}'",
            state.head_name, current
        ))
        .with_hint(format!(
            "switch back to '{}' before continuing or aborting",
            state.head_name
        )));
    }
    Ok(())
}

async fn ensure_expected_head(state: &AmState) -> CliResult<()> {
    let current = Head::current_commit_result()
        .await
        .map_err(|error| am_state_error(format!("failed to resolve HEAD commit: {error}")))?
        .ok_or_else(|| am_state_error("HEAD disappeared during am".to_string()))?;
    if current != state.expected_head {
        return Err(am_head_moved(state, current));
    }
    Ok(())
}

fn am_head_moved(state: &AmState, current: ObjectHash) -> CliError {
    CliError::conflict(format!(
        "branch '{}' moved during am (expected {}, found {})",
        state.head_name,
        short_display_hash(&state.expected_head.to_string()),
        short_display_hash(&current.to_string())
    ))
    .with_hint("restore the expected branch tip before continuing or skipping")
    .with_hint("or run 'libra am --abort' to restore the pre-am tip")
}

/// A crash or write/stage error can leave a new-file patch target untracked.
/// After reset, remove only current-mail targets that the restored index does
/// not own; pre-existing untracked collisions were rejected before am began.
fn cleanup_untracked_patch_targets(targets: &[String]) -> CliResult<()> {
    let index = Index::load(path::index())
        .map_err(|error| am_state_error(format!("failed to load the restored index: {error}")))?;
    let workdir = util::working_dir();
    for target in targets {
        if index.tracked(target, 0) {
            continue;
        }
        let absolute = validate_patch_target(target, &workdir).map_err(|error| {
            am_state_error(format!(
                "refusing to clean unsafe patch target '{target}': {error}"
            ))
        })?;
        match fs::symlink_metadata(&absolute) {
            Ok(metadata) if metadata.is_dir() => {
                return Err(am_state_error(format!(
                    "cannot clean patch target '{}': it became a directory",
                    target
                )));
            }
            Ok(_) => fs::remove_file(&absolute).map_err(|error| {
                am_state_error(format!("failed to clean patch target '{target}': {error}"))
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(am_state_error(format!(
                    "failed to inspect patch target '{target}': {error}"
                )));
            }
        }

        let mut parent = absolute.parent();
        while let Some(directory) = parent {
            if directory == workdir || !directory.starts_with(&workdir) {
                break;
            }
            match fs::remove_dir(directory) {
                Ok(()) => parent = directory.parent(),
                Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => break,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    parent = directory.parent();
                }
                Err(error) => {
                    return Err(am_state_error(format!(
                        "failed to remove empty patch directory '{}': {error}",
                        directory.display()
                    )));
                }
            }
        }
    }
    Ok(())
}

async fn reset_hard(target: &str, output: &OutputConfig) -> CliResult<()> {
    let mut child = output.child_output_config();
    child.quiet = true;
    crate::command::reset::execute_safe(
        crate::command::reset::ResetArgs {
            target: Some(target.to_string()),
            soft: false,
            mixed: false,
            hard: true,
            merge: false,
            keep: false,
            pathspecs: Vec::new(),
            pathspec_separator: false,
            pathspec_from_file: None,
            pathspec_file_nul: false,
            no_refresh: false,
        },
        &child,
    )
    .await
    .map_err(|error| {
        am_state_error(format!(
            "failed to reset am worktree to '{target}': {}",
            error.message()
        ))
    })
}

fn render_output(result: &AmOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("am", result, output);
    }
    if output.quiet {
        return Ok(());
    }
    if result.action == "abort" {
        if let Some(head) = &result.restored_head {
            println!("am aborted; HEAD reset to {}", short_display_hash(head));
        } else {
            println!("am aborted");
        }
        return Ok(());
    }
    for applied in &result.applied {
        println!("Applying: {}", applied.subject);
    }
    if result.action == "skip" && result.applied.is_empty() {
        println!("Skipped the current patch.");
    }
    Ok(())
}

fn invalid_mail(source: &str, detail: &str) -> CliError {
    CliError::fatal(format!("invalid mail patch '{source}': {detail}"))
        .with_stable_code(StableErrorCode::CliInvalidArguments)
}

fn am_state_error(message: String) -> CliError {
    CliError::fatal(message).with_stable_code(StableErrorCode::RepoStateInvalid)
}

fn am_conflict(mail: &MailPatch, detail: String) -> CliError {
    CliError::fatal(format!("patch failed: {}: {detail}", mail.source))
        .with_stable_code(StableErrorCode::ConflictUnresolved)
        .with_hint("resolve the affected files and stage them with 'libra add'")
        .with_hint("then run 'libra am --continue', '--skip', or '--abort'")
}

fn am_resolution_error(message: &str) -> CliError {
    CliError::fatal(message)
        .with_stable_code(StableErrorCode::ConflictUnresolved)
        .with_hint("resolve the current patch and stage only its paths with 'libra add'")
        .with_hint("then run 'libra am --continue', '--skip', or '--abort'")
}

fn mail_subject(message: &str) -> String {
    message.lines().next().unwrap_or("(no subject)").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_format_patch_mail() {
        let mail = "From 0123456789 Mon Sep 17 00:00:00 2001\n\
From: Alice Example <alice@example.com>\n\
Date: Tue, 14 Jul 2026 10:00:00 +0800\n\
Subject: [PATCH 1/1] fix greeting\n\
Content-Type: text/plain; charset=UTF-8\n\
Content-Transfer-Encoding: 8bit\n\
\n\
Explain why.\n\
---\n\
 file.txt | 2 +-\n\
 1 file changed, 1 insertion(+), 1 deletion(-)\n\
\n\
diff --git a/file.txt b/file.txt\n\
--- a/file.txt\n\
+++ b/file.txt\n\
@@ -1 +1 @@\n\
-old\n\
+new\n\
-- \n\
libra 0.18.83\n";
        let parsed = parse_mail_patch("one.patch", mail).expect("parse mail");
        assert_eq!(parsed.author, "Alice Example <alice@example.com>");
        assert_eq!(parsed.message, "fix greeting\n\nExplain why.");
        assert_eq!(parsed.targets, vec!["file.txt"]);
        assert!(!parsed.patch.contains("libra 0.18.83"));
    }

    #[test]
    fn decodes_quoted_printable_and_encoded_subject() {
        assert_eq!(
            decode_quoted_printable("hello=20world=0A").expect("decode"),
            "hello world\n"
        );
        assert_eq!(
            decode_encoded_words("=?UTF-8?Q?fix=3A_caf=C3=A9?=").expect("decode"),
            "fix: café"
        );
    }

    #[test]
    fn cleans_only_patch_subject_prefix() {
        assert_eq!(clean_patch_subject("[PATCH v2 2/3] topic"), "topic");
        assert_eq!(clean_patch_subject("[RFC] topic"), "[RFC] topic");
        assert_eq!(clean_patch_subject("[dispatch] topic"), "[dispatch] topic");
    }

    #[test]
    fn rejects_unsupported_content_types_and_header_injection() {
        assert!(validate_content_type("text/plain; charset=UTF-8").is_ok());
        assert!(validate_content_type("multipart/mixed; boundary=x").is_err());
        assert!(validate_content_type("text/plain; charset=iso-8859-1").is_err());

        let decoded = decode_encoded_words("=?UTF-8?B?QWxpY2UK?= <alice@example.com>")
            .expect("decode injected header");
        assert!(validate_author(&decoded).is_err());
        assert!(validate_author("Alice Example <alice@example.com>").is_ok());
        assert!(validate_author("Alice <alias <alice@example.com>").is_err());
    }
}
