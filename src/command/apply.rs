//! `libra apply --check` — validate that a unified-diff patch applies cleanly,
//! without writing anything. A focused MVP of `git apply --check` built on the
//! same `diffy` patch engine used elsewhere.
//!
//! Only `--check` is supported in this version: the patch is parsed, every
//! target path is safety-checked, and each file hunk-set is test-applied
//! against the current working tree. Actually writing the result (atomic
//! temp-file + rename) is a documented future extension.

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

use clap::Parser;
use serde::Serialize;

use crate::utils::{
    atomic_write::write_atomic,
    error::{CliError, CliResult, StableErrorCode},
    output::{OutputConfig, emit_json_data},
    util,
};

/// Hard cap on a patch input, matching the grit-gap plan's default.
pub(crate) const MAX_PATCH_BYTES: usize = 64 * 1024 * 1024;

/// A fully validated patch result. No worktree write happens while this value
/// is built, so a malformed or non-applicable multi-file patch fails before
/// touching its first target.
#[derive(Debug)]
pub(crate) struct PreparedPatch {
    files: Vec<PreparedFile>,
}

#[derive(Debug)]
struct PreparedFile {
    target: String,
    absolute: PathBuf,
    content: Option<String>,
    permissions: Option<fs::Permissions>,
}

#[derive(Debug)]
pub(crate) enum PatchPreparationError {
    Invalid(String),
    DoesNotApply(String),
}

impl PreparedPatch {
    pub(crate) fn targets(&self) -> Vec<String> {
        self.files.iter().map(|file| file.target.clone()).collect()
    }

    /// Materialize every prepared result. Each replacement is atomic; callers
    /// that need multi-file rollback must persist sequencer state before this
    /// method and restore the current HEAD/index on failure.
    pub(crate) fn write(self) -> Result<(), String> {
        for file in self.files {
            match file.content {
                Some(content) => {
                    write_atomic(&file.absolute, content.as_bytes(), false).map_err(|error| {
                        format!("failed to write patched file '{}': {error}", file.target)
                    })?;
                    if let Some(permissions) = file.permissions {
                        fs::set_permissions(&file.absolute, permissions).map_err(|error| {
                            format!(
                                "failed to restore permissions on patched file '{}': {error}",
                                file.target
                            )
                        })?;
                    }
                }
                None => match fs::remove_file(&file.absolute) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(format!(
                            "failed to remove patched file '{}': {error}",
                            file.target
                        ));
                    }
                },
            }
        }
        Ok(())
    }
}

/// `--help` examples (cross-cutting EXAMPLES contract, `_general.md`).
pub const APPLY_EXAMPLES: &str = "\
EXAMPLES:
    libra apply --check fix.patch            Check whether a patch applies cleanly
    libra apply --check -p0 fix.patch        Do not strip a leading path component
    cat fix.patch | libra apply --check      Read the patch from stdin
    libra --json apply --check fix.patch     Structured { applies, files }";

/// Validate that a unified-diff patch applies (`--check` only in this version).
#[derive(Parser, Debug)]
#[command(after_help = APPLY_EXAMPLES)]
pub struct ApplyArgs {
    /// Check whether the patch applies, without writing. Required in this
    /// version (actually applying the patch is not yet supported).
    #[clap(long)]
    pub check: bool,

    /// Strip `<n>` leading path components from each patched path (like
    /// `git apply -p<n>`; default 1).
    #[clap(short = 'p', value_name = "N", default_value_t = 1)]
    pub strip: u32,

    /// Patch files to read; if none are given, the patch is read from stdin.
    #[clap(value_name = "PATCH")]
    pub patches: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ApplyOutput {
    /// Whether the whole patch applies cleanly.
    applies: bool,
    /// The target paths the patch touches.
    files: Vec<String>,
}

pub async fn execute(args: ApplyArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
        std::process::exit(err.exit_code());
    }
}

/// Safe entry point. Exit 0 when the patch applies, 1 when it does not, 128 on
/// errors (not a repo, unsupported mode, unreadable/oversized/malformed patch,
/// or an unsafe target path).
pub async fn execute_safe(args: ApplyArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;

    let error = |message: String| {
        CliError::fatal(message)
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::CliInvalidArguments)
    };

    if !args.check {
        return Err(error(
            "this version of `apply` only supports --check; pass --check to validate a patch"
                .to_string(),
        ));
    }

    let patch_text = read_patch(&args.patches).map_err(error)?;
    let workdir = util::working_dir();

    let prepared = prepare_patch(&patch_text, args.strip, &workdir);
    let (applies, files) = match prepared {
        Ok(prepared) => (true, prepared.targets()),
        Err(PatchPreparationError::DoesNotApply(_)) => {
            let files = patch_targets(&patch_text, args.strip).map_err(error)?;
            (false, files)
        }
        Err(PatchPreparationError::Invalid(message)) => return Err(error(message)),
    };

    if output.is_json() {
        emit_json_data("apply", &ApplyOutput { applies, files }, output)?;
    }

    if applies {
        Ok(())
    } else {
        // `--check` reports "does not apply" with exit 1 (Git-compatible); the
        // working tree was never touched.
        Err(CliError::silent_exit(1))
    }
}

/// Parse and test-apply a unified diff against `workdir`, returning the final
/// contents for a later write. File sections targeting the same path are
/// applied in order against the preceding section's in-memory result.
pub(crate) fn prepare_patch(
    patch_text: &str,
    strip: u32,
    workdir: &Path,
) -> Result<PreparedPatch, PatchPreparationError> {
    let mut order = Vec::new();
    let mut results: HashMap<String, (PathBuf, Option<String>, Option<fs::Permissions>)> =
        HashMap::new();

    for section in split_file_patches(patch_text) {
        let patch = diffy::Patch::from_str(&section)
            .map_err(|error| PatchPreparationError::Invalid(format!("malformed patch: {error}")))?;
        let target = patch_target(&patch, strip).map_err(PatchPreparationError::Invalid)?;
        let absolute =
            validate_patch_target(&target, workdir).map_err(PatchPreparationError::Invalid)?;

        let is_new_file = patch.original() == Some("/dev/null");
        let is_deletion = patch.modified() == Some("/dev/null");
        let (base, permissions) = match results.get(&target) {
            Some((_, Some(content), permissions)) => (content.clone(), permissions.clone()),
            Some((_, None, permissions)) if is_new_file => (String::new(), permissions.clone()),
            Some((_, None, _)) => {
                return Err(PatchPreparationError::DoesNotApply(format!(
                    "patch target '{target}' was deleted by an earlier section"
                )));
            }
            None if is_new_file => match fs::symlink_metadata(&absolute) {
                Ok(_) => {
                    return Err(PatchPreparationError::DoesNotApply(format!(
                        "new-file patch target '{target}' already exists"
                    )));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => (String::new(), None),
                Err(error) => {
                    return Err(PatchPreparationError::Invalid(format!(
                        "cannot inspect patch target '{target}': {error}"
                    )));
                }
            },
            None => {
                let permissions = fs::symlink_metadata(&absolute)
                    .map_err(|error| {
                        PatchPreparationError::DoesNotApply(format!(
                            "cannot inspect patch target '{target}': {error}"
                        ))
                    })?
                    .permissions();
                let content = fs::read_to_string(&absolute).map_err(|error| {
                    PatchPreparationError::DoesNotApply(format!(
                        "cannot read patch target '{target}': {error}"
                    ))
                })?;
                (content, Some(permissions))
            }
        };

        let result = diffy::apply(&base, &patch).map_err(|_| {
            PatchPreparationError::DoesNotApply(format!("patch does not apply to '{target}'"))
        })?;
        if is_deletion && !result.is_empty() {
            return Err(PatchPreparationError::DoesNotApply(format!(
                "deletion patch did not remove all content from '{target}'"
            )));
        }

        if !results.contains_key(&target) {
            order.push(target.clone());
        }
        results.insert(
            target,
            (
                absolute,
                if is_deletion { None } else { Some(result) },
                permissions,
            ),
        );
    }

    let files = order
        .into_iter()
        .filter_map(|target| {
            results
                .remove(&target)
                .map(|(absolute, content, permissions)| PreparedFile {
                    target,
                    absolute,
                    content,
                    permissions,
                })
        })
        .collect();
    Ok(PreparedPatch { files })
}

/// Return every safe target named by a patch without reading the worktree.
pub(crate) fn patch_targets(patch_text: &str, strip: u32) -> Result<Vec<String>, String> {
    let mut targets = Vec::new();
    let mut seen = HashSet::new();
    for section in split_file_patches(patch_text) {
        let patch = diffy::Patch::from_str(&section)
            .map_err(|error| format!("malformed patch: {error}"))?;
        let target = patch_target(&patch, strip)?;
        resolve_safe(&target, &util::working_dir())?;
        if seen.insert(target.clone()) {
            targets.push(target);
        }
    }
    Ok(targets)
}

/// Read the patch from the given files (concatenated) or from stdin, enforcing
/// the size cap.
fn read_patch(patches: &[String]) -> Result<String, String> {
    let mut buffer = Vec::new();
    if patches.is_empty() {
        io::stdin()
            .take(MAX_PATCH_BYTES as u64 + 1)
            .read_to_end(&mut buffer)
            .map_err(|err| format!("failed to read patch from stdin: {err}"))?;
    } else {
        for path in patches {
            let bytes =
                fs::read(path).map_err(|err| format!("cannot read patch '{path}': {err}"))?;
            buffer.extend_from_slice(&bytes);
            if buffer.len() > MAX_PATCH_BYTES {
                break;
            }
        }
    }
    if buffer.len() > MAX_PATCH_BYTES {
        return Err(format!(
            "patch exceeds the {} MiB limit",
            MAX_PATCH_BYTES / (1024 * 1024)
        ));
    }
    String::from_utf8(buffer).map_err(|_| "patch is not valid UTF-8".to_string())
}

/// Split a (possibly multi-file) unified diff into per-file sections. Git-style
/// diffs split on `diff --git `; plain diffs split on a `--- ` line immediately
/// followed by `+++ ` (so a `--- ...` content-removal line is not mistaken for a
/// file header).
fn split_file_patches(patch: &str) -> Vec<String> {
    let lines: Vec<&str> = patch.lines().collect();
    let git_style = lines.iter().any(|line| line.starts_with("diff --git "));

    let mut starts: Vec<usize> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let is_start = if git_style {
            line.starts_with("diff --git ")
        } else {
            line.starts_with("--- ")
                && lines
                    .get(index + 1)
                    .is_some_and(|next| next.starts_with("+++ "))
        };
        if is_start {
            starts.push(index);
        }
    }

    if starts.is_empty() {
        return vec![patch.to_string()];
    }

    let mut sections = Vec::new();
    for (i, &start) in starts.iter().enumerate() {
        let end = starts.get(i + 1).copied().unwrap_or(lines.len());
        sections.push(format!("{}\n", lines[start..end].join("\n")));
    }
    sections
}

/// Resolve the target path of a file patch (the modified side, or the original
/// side for a deletion), stripping `strip` leading components.
fn patch_target(patch: &diffy::Patch<'_, str>, strip: u32) -> Result<String, String> {
    let deleted = patch.modified() == Some("/dev/null");
    let raw = if deleted {
        patch.original()
    } else {
        patch.modified()
    };
    let raw = raw.ok_or_else(|| "patch is missing a target filename".to_string())?;
    // Validate the RAW path before `-p<n>` stripping: an absolute path must be
    // rejected even when stripping would turn it into a relative one (e.g.
    // `/abs/file` with `-p1` -> `abs/file`).
    if Path::new(raw).is_absolute() || raw.contains('\0') {
        return Err(format!("refusing absolute or NUL patch path '{raw}'"));
    }
    strip_path(raw, strip)
        .ok_or_else(|| format!("cannot strip {strip} path component(s) from '{raw}'"))
}

/// Strip `n` leading slash-separated components from a patch path.
fn strip_path(path: &str, n: u32) -> Option<String> {
    let components: Vec<&str> = path.split('/').collect();
    if components.len() <= n as usize {
        return None;
    }
    Some(components[n as usize..].join("/"))
}

/// Resolve a stripped patch path against the worktree and reject anything that
/// escapes it, contains a NUL, or points inside `.libra/`.
fn resolve_safe(target: &str, workdir: &Path) -> Result<PathBuf, String> {
    if target.is_empty() || target.contains('\0') {
        return Err(format!("invalid target path '{target}'"));
    }
    let target_path = Path::new(target);
    if target_path.is_absolute()
        || target
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
        || target_path.components().any(|component| {
            matches!(
                component,
                Component::Prefix(_)
                    | Component::RootDir
                    | Component::ParentDir
                    | Component::CurDir
            )
        })
    {
        return Err(format!(
            "refusing non-canonical or outside-worktree patch path: '{target}'"
        ));
    }
    let targets_internal_storage = target_path.components().next().is_some_and(|component| {
        matches!(
            component,
            Component::Normal(name)
                if name
                    .to_str()
                    .is_some_and(|name| name.eq_ignore_ascii_case(util::ROOT_DIR))
        )
    });
    if targets_internal_storage {
        return Err(format!(
            "refusing to patch inside {}: '{target}'",
            util::ROOT_DIR
        ));
    }
    let absolute = workdir.join(target_path);
    if !util::is_sub_path(&absolute, workdir) {
        return Err(format!(
            "refusing to patch outside the working tree: '{target}'"
        ));
    }
    Ok(absolute)
}

/// Validate a previously parsed target against the current worktree path
/// topology. Sequencer cleanup re-runs this because an ancestor can be replaced
/// with a symlink while an operation is stopped.
pub(crate) fn validate_patch_target(target: &str, workdir: &Path) -> Result<PathBuf, String> {
    let absolute = resolve_safe(target, workdir)?;
    reject_symlink_components(target, workdir)?;
    Ok(absolute)
}

/// Refuse existing symlinks in a patch path. Lexical containment alone is not
/// sufficient for a writing caller because `dir/link/file` could otherwise
/// escape the worktree through `link`.
fn reject_symlink_components(target: &str, workdir: &Path) -> Result<(), String> {
    let mut current = workdir.to_path_buf();
    for component in Path::new(target).components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "refusing symlink patch path '{}': '{}' is a symlink",
                    target,
                    current.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(format!(
                    "cannot inspect patch path '{}': {error}",
                    current.display()
                ));
            }
        }
    }
    Ok(())
}
