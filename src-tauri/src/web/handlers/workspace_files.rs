//! HTTP endpoints for uploading/downloading workspace files.
//!
//! These exist for issue #179 — the web/server build has no native file
//! dialogs, so the file-tree context menu needs network endpoints to move
//! bytes between the operator's browser and the workspace on disk. The
//! Tauri build keeps using the OS file picker, so the routes are gated to
//! web mode in the UI but live in the shared router so the desktop's
//! built-in web service is functional too.
//!
//! All three endpoints share the same path-safety contract: caller passes
//! a `root_path` (the absolute path of an opened workspace) plus a
//! relative path that must not contain `..` or absolute components. The
//! handler joins them, then `canonicalize`s and confirms the resolved
//! path starts with the canonical root, so a symlink inside the user's
//! workspace cannot redirect reads or writes outside it.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, OnceLock};

use axum::body::{Body, Bytes};
use axum::extract::Multipart;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::stream;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Semaphore;

use crate::app_error::{AppCommandError, UPLOAD_I18N_KEY_TOO_LARGE};

/// Per-file cap for workspace uploads. Larger than the chat-attachment
/// cap (2 MiB) because these bytes go straight to disk in the user's
/// workspace, not into an AI model's context window. Operators can
/// override via `CODEG_WORKSPACE_UPLOAD_MAX_BYTES`.
const WORKSPACE_UPLOAD_DEFAULT_MAX_BYTES: u64 = 500 * 1024 * 1024;
const WORKSPACE_UPLOAD_MAX_BYTES_ENV: &str = "CODEG_WORKSPACE_UPLOAD_MAX_BYTES";

/// Cap on the uncompressed source bytes scanned for a directory ZIP
/// download. The archive is buffered to a temp file on disk (not RAM)
/// before streaming, so this cap mostly protects against runaway disk
/// usage and walk time under concurrency rather than RSS. Lowered from
/// 1 GiB to 256 MiB to fit ordinary remote-server hosts; operators with
/// larger workspaces raise it via `CODEG_WORKSPACE_DOWNLOAD_MAX_BYTES`.
const WORKSPACE_DOWNLOAD_DEFAULT_MAX_BYTES: u64 = 256 * 1024 * 1024;
const WORKSPACE_DOWNLOAD_MAX_BYTES_ENV: &str = "CODEG_WORKSPACE_DOWNLOAD_MAX_BYTES";

/// Cap on concurrent in-flight directory ZIP builds. The zip task pins
/// a blocking thread plus a temp file; without this limit a handful of
/// large-tree requests could saturate the blocking pool and exhaust
/// disk. Override with `CODEG_WORKSPACE_DOWNLOAD_MAX_CONCURRENCY`.
const WORKSPACE_DOWNLOAD_DEFAULT_CONCURRENCY: usize = 2;
const WORKSPACE_DOWNLOAD_CONCURRENCY_ENV: &str =
    "CODEG_WORKSPACE_DOWNLOAD_MAX_CONCURRENCY";

pub fn workspace_upload_max_bytes() -> u64 {
    std::env::var(WORKSPACE_UPLOAD_MAX_BYTES_ENV)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(WORKSPACE_UPLOAD_DEFAULT_MAX_BYTES)
}

pub fn workspace_download_max_bytes() -> u64 {
    std::env::var(WORKSPACE_DOWNLOAD_MAX_BYTES_ENV)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(WORKSPACE_DOWNLOAD_DEFAULT_MAX_BYTES)
}

fn zip_semaphore() -> Arc<Semaphore> {
    static SEM: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SEM.get_or_init(|| {
        let n = std::env::var(WORKSPACE_DOWNLOAD_CONCURRENCY_ENV)
            .ok()
            .and_then(|s| s.trim().parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(WORKSPACE_DOWNLOAD_DEFAULT_CONCURRENCY);
        Arc::new(Semaphore::new(n))
    })
    .clone()
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadWorkspaceFileResult {
    pub path: String,
    pub name: String,
    pub size: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadWorkspaceParams {
    pub root_path: String,
    pub path: String,
}

// ---------------------------------------------------------------------------
// Path safety helpers
// ---------------------------------------------------------------------------

fn validate_relative_components(rel: &Path) -> Result<(), AppCommandError> {
    if rel.is_absolute() {
        return Err(AppCommandError::invalid_input("Path must be relative"));
    }
    for component in rel.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => {
                return Err(AppCommandError::invalid_input("Path cannot contain '..'"));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(AppCommandError::invalid_input("Invalid path component"));
            }
        }
    }
    Ok(())
}

fn resolve_relative_path(root: &Path, rel: &str) -> Result<PathBuf, AppCommandError> {
    let rel_path = Path::new(rel);
    validate_relative_components(rel_path)?;
    Ok(root.join(rel_path))
}

fn ensure_inside_root(root: &Path, target: &Path) -> Result<(), AppCommandError> {
    let canonical_root = std::fs::canonicalize(root).map_err(AppCommandError::io)?;
    let canonical_target = std::fs::canonicalize(target).map_err(AppCommandError::io)?;
    if !canonical_target.starts_with(&canonical_root) {
        return Err(AppCommandError::invalid_input(
            "Resolved path escapes workspace root",
        ));
    }
    Ok(())
}

/// Walk from `root` toward `target` one segment at a time and reject if any
/// already-existing component is a symlink. `target` must be a descendant of
/// `root` (callers compose it via `resolve_relative_path`).
///
/// This runs *before* `create_dir_all`, which would otherwise follow a
/// symlink mid-chain and silently create new directories outside the
/// workspace. The earlier post-hoc `canonicalize` check caught the
/// escape but the side-effect (empty dir at the symlink target) was
/// already on disk.
fn ensure_no_symlink_in_chain(root: &Path, target: &Path) -> Result<(), AppCommandError> {
    let rel = target.strip_prefix(root).map_err(|_| {
        AppCommandError::invalid_input("Target path is not under workspace root")
    })?;
    let mut current = root.to_path_buf();
    for component in rel.components() {
        let segment = match component {
            Component::Normal(s) => s,
            Component::CurDir => continue,
            _ => {
                return Err(AppCommandError::invalid_input(
                    "Invalid path component while validating upload target",
                ));
            }
        };
        current.push(segment);
        match std::fs::symlink_metadata(&current) {
            Ok(md) => {
                if md.file_type().is_symlink() {
                    return Err(AppCommandError::invalid_input(
                        "Upload path traverses a symlink; refuse to follow it",
                    ));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // The remainder of the path doesn't exist yet — nothing
                // for create_dir_all to follow into, so we're safe.
                return Ok(());
            }
            Err(e) => return Err(AppCommandError::io(e)),
        }
    }
    Ok(())
}

/// Strip cross-platform-hostile characters from a single path segment.
/// Empty / all-dots input collapses to `"file"` so the rename can succeed
/// even when the browser hands us a degenerate name.
fn sanitize_segment(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            other => other,
        })
        .collect();
    let trimmed = cleaned
        .trim_matches(|c: char| c.is_whitespace())
        .trim_end_matches('.');
    if trimmed.is_empty() || trimmed.chars().all(|c| c == '.') {
        "file".to_string()
    } else {
        trimmed.to_string()
    }
}

fn sanitize_relative_subpath(raw: &str) -> Result<String, AppCommandError> {
    let raw_parts: Vec<&str> = raw
        .split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    if raw_parts.is_empty() {
        return Err(AppCommandError::invalid_input("Invalid upload path"));
    }
    // Reject parent-dir traversal *before* `sanitize_segment` collapses
    // it to "file" — otherwise the check would never fire and a request
    // for `../escape` would silently rewrite to `file/escape`, hiding the
    // operator's intent (and surprising whoever audits the resulting
    // path on disk).
    if raw_parts.contains(&"..") {
        return Err(AppCommandError::invalid_input("Path cannot contain '..'"));
    }
    let parts: Vec<String> = raw_parts.iter().map(|s| sanitize_segment(s)).collect();
    Ok(parts.join("/"))
}

fn header_safe_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_control() || c == '"' || c == '\\' {
                '_'
            } else if c.is_ascii() {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn attachment_header(name: &str) -> Option<HeaderValue> {
    HeaderValue::from_str(&format!(
        "attachment; filename=\"{}\"; filename*=UTF-8''{}",
        header_safe_filename(name),
        urlencoding::encode(name)
    ))
    .ok()
}

// ---------------------------------------------------------------------------
// Upload
// ---------------------------------------------------------------------------

/// Stream a single file from the operator's browser into the workspace.
///
/// Expected multipart fields (order matters — text fields must precede
/// `file` so the handler can resolve the destination before any bytes
/// land on disk):
///   * `root_path` — absolute path of the opened workspace folder
///   * `target_path` — relative directory under `root_path` to upload
///     into. Empty / missing means workspace root.
///   * `relative_path` — optional relative path *including filename*
///     used for folder uploads to preserve directory structure. When
///     present, the browser's filename is ignored.
///   * `file` — the file payload.
pub async fn upload_workspace_file(
    mut multipart: Multipart,
) -> Result<Json<UploadWorkspaceFileResult>, AppCommandError> {
    let mut root_path: Option<String> = None;
    let mut target_path: Option<String> = None;
    let mut relative_path: Option<String> = None;
    let mut result: Option<UploadWorkspaceFileResult> = None;
    let max_bytes = workspace_upload_max_bytes();

    while let Some(mut field) = multipart.next_field().await.map_err(|e| {
        AppCommandError::io_error("Invalid multipart upload").with_detail(e.to_string())
    })? {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "root_path" | "rootPath" => {
                root_path = Some(field.text().await.map_err(|e| {
                    AppCommandError::io_error("Failed to read root_path field")
                        .with_detail(e.to_string())
                })?);
            }
            "target_path" | "targetPath" => {
                target_path = Some(field.text().await.map_err(|e| {
                    AppCommandError::io_error("Failed to read target_path field")
                        .with_detail(e.to_string())
                })?);
            }
            "relative_path" | "relativePath" => {
                relative_path = Some(field.text().await.map_err(|e| {
                    AppCommandError::io_error("Failed to read relative_path field")
                        .with_detail(e.to_string())
                })?);
            }
            "file" => {
                if result.is_some() {
                    return Err(AppCommandError::invalid_input(
                        "Multiple `file` fields are not supported per request",
                    ));
                }
                let root_str = root_path.as_deref().ok_or_else(|| {
                    AppCommandError::invalid_input(
                        "root_path field must appear before the file field",
                    )
                })?;
                let root = PathBuf::from(root_str);
                if !root.exists() || !root.is_dir() {
                    return Err(AppCommandError::not_found(
                        "Workspace folder does not exist",
                    ));
                }
                let canonical_root =
                    std::fs::canonicalize(&root).map_err(AppCommandError::io)?;

                let file_name_hint = field
                    .file_name()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "file".to_string());

                let final_rel = compute_final_rel(
                    target_path.as_deref().unwrap_or(""),
                    relative_path.as_deref().unwrap_or(""),
                    &file_name_hint,
                )?;
                let final_abs = resolve_relative_path(&root, &final_rel)?;

                if let Some(parent) = final_abs.parent() {
                    // Reject *before* touching the filesystem if any
                    // existing component along the path is a symlink —
                    // otherwise `create_dir_all` would follow the link
                    // and create directories outside the workspace
                    // before the canonical check below could fire.
                    ensure_no_symlink_in_chain(&root, parent)?;
                    tokio::fs::create_dir_all(parent).await.map_err(|e| {
                        AppCommandError::io_error("Failed to create upload directory")
                            .with_detail(e.to_string())
                    })?;
                    let canonical_parent =
                        std::fs::canonicalize(parent).map_err(AppCommandError::io)?;
                    if !canonical_parent.starts_with(&canonical_root) {
                        return Err(AppCommandError::invalid_input(
                            "Resolved path escapes workspace root",
                        ));
                    }
                }

                if final_abs.is_dir() {
                    return Err(AppCommandError::invalid_input(
                        "Refusing to overwrite an existing directory with a file",
                    ));
                }
                if final_abs.exists() {
                    return Err(AppCommandError::already_exists(
                        "A file with this name already exists",
                    ));
                }

                let staging_name = format!(
                    ".codeg-upload-{}.part",
                    uuid::Uuid::new_v4().simple()
                );
                let staging_path = final_abs
                    .parent()
                    .map(|p| p.join(&staging_name))
                    .ok_or_else(|| {
                        AppCommandError::invalid_input("Cannot determine parent directory")
                    })?;

                let mut out = tokio::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&staging_path)
                    .await
                    .map_err(|e| {
                        AppCommandError::io_error("Failed to create staging file")
                            .with_detail(e.to_string())
                    })?;

                let mut written: u64 = 0;
                let stream_result: Result<(), AppCommandError> = async {
                    while let Some(chunk) = field.chunk().await.map_err(|e| {
                        AppCommandError::io_error("Failed to read upload chunk")
                            .with_detail(e.to_string())
                    })? {
                        let new_total = written.saturating_add(chunk.len() as u64);
                        if new_total > max_bytes {
                            let mut params = BTreeMap::new();
                            params.insert("size".to_string(), new_total.to_string());
                            params.insert("limit".to_string(), max_bytes.to_string());
                            return Err(AppCommandError::io_error(
                                "Upload exceeds the maximum allowed size",
                            )
                            .with_detail(format!("size={new_total} limit={max_bytes}"))
                            .with_i18n(UPLOAD_I18N_KEY_TOO_LARGE, params));
                        }
                        out.write_all(&chunk).await.map_err(|e| {
                            AppCommandError::io_error("Failed to write chunk")
                                .with_detail(e.to_string())
                        })?;
                        written = new_total;
                    }
                    out.flush().await.map_err(|e| {
                        AppCommandError::io_error("Failed to flush staging file")
                            .with_detail(e.to_string())
                    })?;
                    Ok(())
                }
                .await;
                drop(out);

                if let Err(err) = stream_result {
                    let _ = tokio::fs::remove_file(&staging_path).await;
                    return Err(err);
                }

                // Empty files are valid in a workspace (`.gitkeep`,
                // `__init__.py`, placeholder configs) — only chat
                // attachments need the "must contain bytes" guard, since
                // those feed an LLM. Don't reject here.

                // Commit the staging file onto the final name atomically.
                // `hard_link` errors with `AlreadyExists` instead of
                // silently overwriting, which closes the TOCTOU window
                // that a bare `rename` leaves open on Unix (rename(2)
                // replaces an existing destination). On filesystems that
                // don't support hard links (Windows FAT32, cross-device,
                // some FUSE mounts) we fall back to `rename` — that path
                // still has the narrow race but it's the best we can do
                // there, and the user is uploading into their own
                // workspace so the race window has no security impact.
                let commit_method: &str;
                match tokio::fs::hard_link(&staging_path, &final_abs).await {
                    Ok(()) => {
                        commit_method = "hard_link";
                        let _ = tokio::fs::remove_file(&staging_path).await;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        let _ = tokio::fs::remove_file(&staging_path).await;
                        return Err(AppCommandError::already_exists(
                            "A file with this name already exists",
                        ));
                    }
                    Err(hard_link_err) => {
                        if let Err(e) =
                            tokio::fs::rename(&staging_path, &final_abs).await
                        {
                            let _ = tokio::fs::remove_file(&staging_path).await;
                            return Err(AppCommandError::io_error(
                                "Failed to commit upload",
                            )
                            .with_detail(format!(
                                "hard_link_err={hard_link_err} rename_err={e}"
                            )));
                        }
                        commit_method = "rename";
                    }
                }

                // Defense in depth: re-check that the committed path is
                // inside the root. If a symlink got swapped under us, undo.
                if let Err(err) = ensure_inside_root(&root, &final_abs) {
                    let _ = tokio::fs::remove_file(&final_abs).await;
                    return Err(err);
                }

                // Sanity verification: the API has been observed to
                // return success while leaving nothing on disk. Stat the
                // final path BEFORE responding so a regression surfaces
                // as an error here instead of as a phantom file in the
                // tree that delete/edit can't touch. Use symlink_metadata
                // (NOT exists()) so a dangling link is detected too.
                match tokio::fs::symlink_metadata(&final_abs).await {
                    Ok(_) => {}
                    Err(err) => {
                        eprintln!(
                            "[workspace_files] upload commit verification FAILED: \
                             final_abs={} commit_method={} written={} err={}",
                            final_abs.display(),
                            commit_method,
                            written,
                            err
                        );
                        return Err(AppCommandError::io_error(
                            "Upload appeared to succeed but the file is missing",
                        )
                        .with_detail(format!(
                            "final_abs={} commit_method={} err={}",
                            final_abs.display(),
                            commit_method,
                            err
                        )));
                    }
                }

                let name = final_abs
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("file")
                    .to_string();

                result = Some(UploadWorkspaceFileResult {
                    path: final_rel,
                    name,
                    size: written,
                });
            }
            _ => {
                // Drain unknown fields to keep the parser moving.
                let _ = field.bytes().await;
            }
        }
    }

    result
        .ok_or_else(|| AppCommandError::invalid_input("Missing `file` field"))
        .map(Json)
}

fn compute_final_rel(
    target_dir: &str,
    relative_path: &str,
    file_name_hint: &str,
) -> Result<String, AppCommandError> {
    let target_dir_clean = target_dir.trim().trim_end_matches(['/', '\\']);
    let body = if !relative_path.trim().is_empty() {
        sanitize_relative_subpath(relative_path)?
    } else {
        let last = file_name_hint
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(file_name_hint);
        sanitize_segment(last)
    };
    let combined = if target_dir_clean.is_empty() {
        body
    } else {
        let dir = sanitize_relative_subpath(target_dir_clean)?;
        format!("{dir}/{body}")
    };
    // Final sanity check — re-validate the joined path as relative
    // components only.
    validate_relative_components(Path::new(&combined))?;
    Ok(combined)
}

// ---------------------------------------------------------------------------
// Download (single file)
// ---------------------------------------------------------------------------

pub async fn download_workspace_file(
    Json(params): Json<DownloadWorkspaceParams>,
) -> Result<Response, AppCommandError> {
    let root = PathBuf::from(&params.root_path);
    if !root.exists() || !root.is_dir() {
        return Err(AppCommandError::not_found(
            "Workspace folder does not exist",
        ));
    }
    let target = resolve_relative_path(&root, &params.path)?;
    if !target.exists() {
        return Err(AppCommandError::not_found("File does not exist"));
    }
    if !target.is_file() {
        return Err(AppCommandError::invalid_input("Path is not a file"));
    }
    ensure_inside_root(&root, &target)?;

    let metadata = tokio::fs::metadata(&target)
        .await
        .map_err(AppCommandError::io)?;
    let size = metadata.len();
    let file = tokio::fs::File::open(&target)
        .await
        .map_err(AppCommandError::io)?;

    let body_stream = stream::unfold(file, |mut file| async move {
        let mut buf = vec![0u8; 64 * 1024];
        match file.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                let bytes: Bytes = buf.into();
                Some((Ok::<_, std::io::Error>(bytes), file))
            }
            Err(e) => Some((Err(e), file)),
        }
    });
    let body = Body::from_stream(body_stream);

    let name = target
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("download")
        .to_string();

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    if let Ok(v) = HeaderValue::from_str(&size.to_string()) {
        headers.insert(header::CONTENT_LENGTH, v);
    }
    if let Some(v) = attachment_header(&name) {
        headers.insert(header::CONTENT_DISPOSITION, v);
    }

    Ok((StatusCode::OK, headers, body).into_response())
}

// ---------------------------------------------------------------------------
// Download (directory as ZIP)
// ---------------------------------------------------------------------------

pub async fn download_workspace_dir(
    Json(params): Json<DownloadWorkspaceParams>,
) -> Result<Response, AppCommandError> {
    let root = PathBuf::from(&params.root_path);
    if !root.exists() || !root.is_dir() {
        return Err(AppCommandError::not_found(
            "Workspace folder does not exist",
        ));
    }
    let (dir_path, dir_name) = if params.path.is_empty() {
        let name = root
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("workspace")
            .to_string();
        (root.clone(), name)
    } else {
        let resolved = resolve_relative_path(&root, &params.path)?;
        if !resolved.exists() {
            return Err(AppCommandError::not_found("Directory does not exist"));
        }
        if !resolved.is_dir() {
            return Err(AppCommandError::invalid_input("Path is not a directory"));
        }
        ensure_inside_root(&root, &resolved)?;
        let name = resolved
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("folder")
            .to_string();
        (resolved, name)
    };
    let zip_name = format!("{dir_name}.zip");

    // Bound concurrent zip jobs across the whole process. `acquire_owned`
    // returns an `OwnedSemaphorePermit` we can move into the streaming
    // state below, so the slot stays held until the client finishes
    // draining the response (or hangs up).
    let permit = zip_semaphore()
        .acquire_owned()
        .await
        .map_err(|e| {
            AppCommandError::io_error("Zip concurrency gate closed")
                .with_detail(e.to_string())
        })?;

    let dir_for_blocking = dir_path.clone();
    let max_bytes = workspace_download_max_bytes();
    let (temp_file, content_length): (tempfile::NamedTempFile, u64) =
        tokio::task::spawn_blocking(move || {
            let mut temp = tempfile::NamedTempFile::new().map_err(|e| {
                AppCommandError::io_error("Failed to create zip temp file")
                    .with_detail(e.to_string())
            })?;
            let size = build_zip_archive_to_writer(
                &dir_for_blocking,
                max_bytes,
                temp.as_file_mut(),
            )?;
            Ok::<_, AppCommandError>((temp, size))
        })
        .await
        .map_err(|e| {
            AppCommandError::io_error("Zip task failed").with_detail(e.to_string())
        })??;

    // Re-open async for streaming. On Linux/macOS, holding two
    // handles (the NamedTempFile-internal sync File and this async
    // re-opened File) over the same inode is fine — unlink at stream
    // end works even with handles open, and the inode is reclaimed
    // when both close.
    //
    // **Windows caveat**: NamedTempFile opens with FILE_SHARE_DELETE
    // so the re-open succeeds, but the unlink at NamedTempFile drop
    // requires *all* handles to have been opened with
    // FILE_SHARE_DELETE — `tokio::fs::File::open` (which delegates to
    // `std::fs::File::open`) does NOT set that flag. The unlink call
    // therefore succeeds in the sense that the file is marked for
    // deletion, but the actual removal is deferred until the async
    // handle's close completes. In the normal stream-drain path that
    // close happens microseconds before the NamedTempFile drop (tuple
    // drop order: `file` → `temp_file` → `permit`), so the inode is
    // gone by the time this function returns. The race window only
    // matters if a Windows-hosted codeg-server takes a hard process
    // kill mid-stream; orphaned temp files then sit in `%TEMP%` until
    // the OS scheduled cleanup runs.
    let file = tokio::fs::File::open(temp_file.path())
        .await
        .map_err(AppCommandError::io)?;

    // State carried through the stream: file handle, temp guard
    // (NamedTempFile is unlinked on drop), and the permit (released on
    // drop). All three drop atomically when the stream ends (`None`
    // branch) or the client disconnects, which is exactly the cleanup
    // we want.
    let body_stream = stream::unfold(
        (file, temp_file, permit),
        |(mut file, temp_file, permit)| async move {
            let mut buf = vec![0u8; 64 * 1024];
            match file.read(&mut buf).await {
                Ok(0) => None,
                Ok(n) => {
                    buf.truncate(n);
                    Some((
                        Ok::<_, std::io::Error>(Bytes::from(buf)),
                        (file, temp_file, permit),
                    ))
                }
                Err(e) => Some((Err(e), (file, temp_file, permit))),
            }
        },
    );
    let body = Body::from_stream(body_stream);

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/zip"),
    );
    if let Ok(v) = HeaderValue::from_str(&content_length.to_string()) {
        headers.insert(header::CONTENT_LENGTH, v);
    }
    if let Some(v) = attachment_header(&zip_name) {
        headers.insert(header::CONTENT_DISPOSITION, v);
    }
    Ok((StatusCode::OK, headers, body).into_response())
}

/// Walk `dir` and write a Deflate-compressed zip archive into `sink`.
///
/// Runs on the blocking pool because `walkdir` and `zip::ZipWriter` are
/// sync APIs, and `ZipWriter` requires `Write + Seek` (so we cannot
/// stream into an mpsc channel directly — the caller uses a temp file
/// as the seekable sink instead). `max_bytes` caps the total
/// uncompressed source bytes scanned; exceeding it errors out before
/// any response is sent so the client gets a 400, not a truncated zip.
///
/// Returns the final on-disk archive size in bytes, used for the
/// `Content-Length` response header.
///
/// Symlinks are intentionally skipped (not followed) — `follow_links(false)`
/// reports a symlink as neither file nor directory, and we leave it that
/// way to avoid traversing out of the workspace via a misplaced link.
fn build_zip_archive_to_writer<W: std::io::Write + std::io::Seek>(
    dir: &Path,
    max_bytes: u64,
    sink: W,
) -> Result<u64, AppCommandError> {
    use std::io::Read;
    use zip::write::SimpleFileOptions;

    let mut writer = zip::ZipWriter::new(sink);
    let base_options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    // Directories need the execute bit so extractors set a mode that
    // lets the user `cd` into them and list their contents. The earlier
    // blanket 0o644 produced archives whose extracted subdirectories
    // refused access on Unix, surfacing as "permission denied" when the
    // operator opened the unzipped tree.
    let dir_options = base_options.unix_permissions(0o755);
    let file_options = base_options.unix_permissions(0o644);

    let mut total_source_bytes: u64 = 0;
    let mut symlinks_skipped: u64 = 0;

    for entry in walkdir::WalkDir::new(dir).follow_links(false) {
        let entry = entry.map_err(|e| {
            AppCommandError::io_error("Failed to walk directory").with_detail(e.to_string())
        })?;
        let path = entry.path();
        let rel = match path.strip_prefix(dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let file_type = entry.file_type();
        if file_type.is_symlink() {
            symlinks_skipped = symlinks_skipped.saturating_add(1);
            continue;
        }
        if file_type.is_dir() {
            writer
                .add_directory(format!("{rel_str}/"), dir_options)
                .map_err(|e| {
                    AppCommandError::io_error("Failed to add dir to zip")
                        .with_detail(e.to_string())
                })?;
        } else if file_type.is_file() {
            let metadata = entry.metadata().map_err(|e| {
                AppCommandError::io_error("Failed to stat entry")
                    .with_detail(e.to_string())
            })?;
            total_source_bytes = total_source_bytes.saturating_add(metadata.len());
            if total_source_bytes > max_bytes {
                return Err(AppCommandError::invalid_input(
                    "Directory exceeds the maximum download size",
                )
                .with_detail(format!(
                    "scanned={total_source_bytes} limit={max_bytes}"
                )));
            }
            writer.start_file(&rel_str, file_options).map_err(|e| {
                AppCommandError::io_error("Failed to start zip entry")
                    .with_detail(e.to_string())
            })?;
            let mut f = std::fs::File::open(path).map_err(AppCommandError::io)?;
            let mut buf = [0u8; 64 * 1024];
            loop {
                let n = f.read(&mut buf).map_err(AppCommandError::io)?;
                if n == 0 {
                    break;
                }
                use std::io::Write;
                writer.write_all(&buf[..n]).map_err(|e| {
                    AppCommandError::io_error("Failed to write zip entry")
                        .with_detail(e.to_string())
                })?;
            }
        }
    }
    if symlinks_skipped > 0 {
        eprintln!(
            "[workspace_files] download_workspace_dir: skipped {} symlink entries under {}",
            symlinks_skipped,
            dir.display()
        );
    }
    let mut finished = writer.finish().map_err(|e| {
        AppCommandError::io_error("Failed to finalize zip").with_detail(e.to_string())
    })?;
    let size = finished.stream_position().map_err(|e| {
        AppCommandError::io_error("Failed to measure zip size")
            .with_detail(e.to_string())
    })?;
    Ok(size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_segment_replaces_hostile_chars_and_handles_dots() {
        // sanitize_segment is the *per-segment* sanitizer — it is not
        // expected to extract the basename; that's `sanitize_relative_subpath`'s
        // job. Hostile chars are replaced and degenerate inputs collapse
        // to "file" so the rename succeeds.
        assert_eq!(sanitize_segment("a:b*c?\"d"), "a_b_c__d");
        assert_eq!(sanitize_segment("..."), "file");
        assert_eq!(sanitize_segment(""), "file");
        assert_eq!(sanitize_segment("normal.txt"), "normal.txt");
    }

    #[test]
    fn sanitize_relative_subpath_joins_clean() {
        assert_eq!(
            sanitize_relative_subpath("a/b/c.txt").unwrap(),
            "a/b/c.txt"
        );
        assert_eq!(
            sanitize_relative_subpath("a\\b\\c.txt").unwrap(),
            "a/b/c.txt"
        );
        assert_eq!(sanitize_relative_subpath("./a/./b").unwrap(), "a/b");
    }

    #[test]
    fn sanitize_relative_subpath_rejects_empty_and_traversal() {
        assert!(sanitize_relative_subpath("").is_err());
        assert!(sanitize_relative_subpath("/").is_err());
        assert!(sanitize_relative_subpath("../escape").is_err());
    }

    #[test]
    fn compute_final_rel_uses_file_name_when_no_relative() {
        assert_eq!(
            compute_final_rel("dir", "", "report.txt").unwrap(),
            "dir/report.txt"
        );
        assert_eq!(compute_final_rel("", "", "report.txt").unwrap(), "report.txt");
    }

    #[test]
    fn compute_final_rel_prefers_relative_path() {
        assert_eq!(
            compute_final_rel("dir", "sub/a.txt", "ignored").unwrap(),
            "dir/sub/a.txt"
        );
        assert_eq!(
            compute_final_rel("", "a/b/c.txt", "ignored").unwrap(),
            "a/b/c.txt"
        );
    }

    #[test]
    fn validate_relative_components_rejects_dotdot_and_absolute() {
        assert!(validate_relative_components(Path::new("../escape")).is_err());
        assert!(validate_relative_components(Path::new("/etc/passwd")).is_err());
        assert!(validate_relative_components(Path::new("a/b")).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn ensure_no_symlink_in_chain_rejects_intermediate_symlink() {
        use std::fs;
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("tempdir root");
        let outside = tempfile::tempdir().expect("tempdir outside");

        // root/link -> outside
        symlink(outside.path(), root.path().join("link")).expect("symlink");

        // Target: root/link/sub — does NOT exist, but the intermediate
        // `link` component is a symlink that would carry create_dir_all
        // out of the root.
        let target = root.path().join("link").join("sub");
        let err = ensure_no_symlink_in_chain(root.path(), &target)
            .expect_err("should reject symlink in chain");
        assert!(
            err.message.contains("symlink"),
            "unexpected error: {}",
            err.message
        );

        // Sanity: no symlink in chain → ok.
        fs::create_dir(root.path().join("real")).expect("real dir");
        let ok_target = root.path().join("real").join("nested").join("file.txt");
        assert!(ensure_no_symlink_in_chain(root.path(), &ok_target).is_ok());
    }
}
