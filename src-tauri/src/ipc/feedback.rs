//! In-app feedback: window capture → annotation → a queued TaskYou task.
//!
//! This is the loop that improves the app while it is being used, so it is
//! written to the same standard as the mail path, not as tooling. Two commands:
//!
//! | command | argument | returns |
//! |---|---|---|
//! | `capture_window` | — | `String` — a `data:image/png;base64,…` URL |
//! | `submit_feedback` | `text`, `imagePngBase64?`, `context?` | [`FeedbackReceipt`] |
//!
//! # Capture
//!
//! macOS only, via `/usr/sbin/screencapture`. The app's own window is captured
//! by *rect* — Tauri knows the window's outer position and size, and
//! `screencapture -R` takes exactly that — rather than by window id, which
//! would mean reaching through `NSWindow` for a `windowNumber` and pulling in
//! an Objective-C dependency for one integer. If the rect cannot be resolved it
//! falls back to the main display (`-m`). Requires the Screen Recording
//! permission; without it `screencapture` fails and the message is surfaced.
//!
//! # Filing
//!
//! The annotated PNG is written to `<repo>/.feedback/` — a durable directory,
//! never a temp dir, because the agent reads it minutes later — and then:
//!
//! ```text
//! ty create "<title>" --body "<body>" --project mach --execute
//! ```
//!
//! Everything that decides *what* runs is a pure function ([`derive_title`],
//! [`build_body`], [`ty_args`], [`capture_args`], [`resolve_ty_binary`]) so
//! `tests/feedback.rs` can assert the command line without executing anything.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tauri::Manager;

use super::error::IpcError;

/// Every piece of feedback is filed into this TaskYou project.
pub const TY_PROJECT: &str = "mach";

const PNG_DATA_URL: &str = "data:image/png;base64,";
const SCREENCAPTURE: &str = "/usr/sbin/screencapture";
const TITLE_MAX: usize = 72;

// ===========================================================================
// Payloads
// ===========================================================================

/// Where the user was standing when they hit ⌘K.
///
/// The agent that picks the task up has no conversation context, so "which
/// screen was this" has to travel with the screenshot. Every field is optional:
/// a missing one is left out of the body rather than rendered as "unknown".
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackContext {
    /// `mail` or `calendar`.
    pub mode: Option<String>,
    /// The calendar view, when the calendar was open: `day` / `week` / `month`.
    pub view: Option<String>,
    /// The mailbox filter — a label id, or "all accounts".
    pub label: Option<String>,
    /// The account whose stream was showing, by address.
    pub account: Option<String>,
    /// The subject of the open thread, if one was open.
    pub thread: Option<String>,
}

/// What the confirmation screen renders. Never just "ok".
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackReceipt {
    /// The TaskYou task number, parsed out of `Created task #N: …`.
    pub task_id: Option<u64>,
    /// Absolute path to the annotated PNG that was filed.
    pub screenshot_path: Option<String>,
    /// One sentence for a human.
    pub message: String,
    /// Whatever `ty` actually printed, for when the sentence is not enough.
    pub output: String,
}

/// The window to hand `screencapture -R`, in points, top-left origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Everything [`build_body`] needs. Borrowed, so building a body allocates once.
pub struct FeedbackReport<'a> {
    pub text: &'a str,
    pub screenshot: Option<&'a Path>,
    pub context: Option<&'a FeedbackContext>,
    pub repo_root: &'a Path,
    pub app_version: &'a str,
    pub commit: Option<&'a str>,
}

// ===========================================================================
// Pure parts — everything the tests drive
// ===========================================================================

/// The `screencapture` argv, without the program itself.
///
/// `-x` silences the shutter, `-o` drops the window shadow. A rect is captured
/// when we know one; otherwise the main display, which is honest but includes
/// whatever else is on screen.
pub fn capture_args(rect: Option<WindowRect>, out: &Path) -> Vec<String> {
    let mut args = vec!["-x".to_string(), "-o".to_string()];
    match rect {
        Some(r) if r.width > 0 && r.height > 0 => {
            args.push(format!("-R{},{},{},{}", r.x, r.y, r.width, r.height));
        }
        _ => args.push("-m".to_string()),
    }
    args.push(out.to_string_lossy().into_owned());
    args
}

/// `ty create "<title>" --body "<body>" --project mach --execute`, as argv.
///
/// Built as a vector rather than a shell string on purpose: the body contains
/// newlines, quotes and paths, and none of it is ever handed to a shell.
pub fn ty_args(title: &str, body: &str) -> Vec<String> {
    vec![
        "create".to_string(),
        title.to_string(),
        "--body".to_string(),
        body.to_string(),
        "--project".to_string(),
        TY_PROJECT.to_string(),
        "--execute".to_string(),
    ]
}

/// A task title from the first line of what was typed.
///
/// TaskYou will invent a title from the body if none is given, but that costs a
/// model round trip and this is meant to be instant. The first sentence of
/// "move the account bar to the left, it is too far right" is already the title.
pub fn derive_title(text: &str) -> String {
    let first = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let collapsed = first.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return "Feedback from inside Mach".to_string();
    }
    if collapsed.chars().count() <= TITLE_MAX {
        return collapsed;
    }
    // Cut on a word boundary; a title ending mid-word reads like a bug.
    let head: String = collapsed.chars().take(TITLE_MAX).collect();
    let cut = head.rfind(' ').unwrap_or(head.len());
    format!("{}…", head[..cut].trim_end())
}

/// The task body, written as instructions to an engineer who was not here.
pub fn build_body(report: &FeedbackReport<'_>) -> String {
    let mut out = String::with_capacity(1024);

    out.push_str(
        "The user filed this from inside Mach (⌘K → Send feedback) while using the app.\n\
         Treat it as a work item: make the change, in this repo, and leave the app\n\
         running better than you found it.\n\n",
    );

    out.push_str("## What they asked for\n\n");
    out.push_str(report.text.trim());
    out.push_str("\n\n");

    match report.screenshot {
        Some(path) => {
            out.push_str("## The screenshot\n\n");
            out.push_str(&path.display().to_string());
            out.push_str(
                "\n\nThat image is Mach's own window at the moment they hit ⌘K, with their\n\
                 annotations drawn on top. An arrow or box marks the exact element they\n\
                 mean — read the image before you read the sentence again, it is the\n\
                 more precise half of the request.\n\n",
            );
        }
        None => {
            out.push_str("## The screenshot\n\nNone — this one was filed as text only.\n\n");
        }
    }

    if let Some(context) = report.context {
        let mut lines = Vec::new();
        if let Some(mode) = clean(context.mode.as_deref()) {
            lines.push(format!("- Mode: {mode}"));
        }
        if let Some(view) = clean(context.view.as_deref()) {
            lines.push(format!("- Calendar view: {view}"));
        }
        if let Some(label) = clean(context.label.as_deref()) {
            lines.push(format!("- Mailbox: {label}"));
        }
        if let Some(account) = clean(context.account.as_deref()) {
            lines.push(format!("- Account: {account}"));
        }
        if let Some(thread) = clean(context.thread.as_deref()) {
            lines.push(format!("- Open thread: {thread}"));
        }
        if !lines.is_empty() {
            out.push_str("## What was on screen\n\n");
            out.push_str(&lines.join("\n"));
            out.push_str("\n\n");
        }
    }

    out.push_str("## Where to work\n\n");
    out.push_str(&format!("- Repo: {}\n", report.repo_root.display()));
    out.push_str(&format!("- App version: {}\n", report.app_version));
    if let Some(commit) = report.commit {
        out.push_str(&format!("- Commit at capture time: {commit}\n"));
    }
    out.push_str(
        "\nThe app is running in dev mode. Frontend edits under `src/` hot-reload into\n\
         the open window with no restart and no lost state, so a layout or colour change\n\
         is visible in seconds. Rust edits under `src-tauri/` need a rebuild and a\n\
         relaunch — prefer the frontend fix when both would work.\n",
    );

    out
}

fn clean(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// Decode a PNG that arrived as base64, with or without a `data:` prefix.
pub fn decode_png(data: &str) -> Result<Vec<u8>, IpcError> {
    let trimmed = data.trim();
    let payload = if let Some(rest) = trimmed.strip_prefix(PNG_DATA_URL) {
        rest
    } else if trimmed.starts_with("data:") {
        // Some other data URL — take whatever follows the comma and let the
        // decoder complain if it was not base64 after all.
        trimmed.split_once(',').map(|(_, rest)| rest).unwrap_or("")
    } else {
        trimmed
    };
    if payload.is_empty() {
        return Err(IpcError::internal("the screenshot arrived empty"));
    }
    STANDARD
        .decode(payload.as_bytes())
        .map_err(|e| IpcError::internal(format!("the screenshot was not valid base64: {e}")))
}

/// `Created task #418: …` → `418`.
pub fn parse_task_id(stdout: &str) -> Option<u64> {
    let after_hash = stdout.split('#').nth(1)?;
    let digits: String = after_hash.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Where the `ty` binary might be, best guess first.
///
/// A GUI-launched app inherits `launchd`'s `PATH`, not the shell's, so `ty`
/// being on `PATH` in a terminal proves nothing. Set `MACH_TY_BIN` to an
/// absolute path if the guesses below do not find it.
pub fn ty_candidates() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut push = |path: PathBuf| {
        if !out.contains(&path) {
            out.push(path);
        }
    };

    if let Ok(explicit) = std::env::var("MACH_TY_BIN") {
        if !explicit.trim().is_empty() {
            push(PathBuf::from(explicit.trim()));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        push(Path::new(&home).join("Projects/workflow/bin/ty"));
    }
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            push(dir.join("ty"));
        }
    }
    push(PathBuf::from("/opt/homebrew/bin/ty"));
    push(PathBuf::from("/usr/local/bin/ty"));
    out
}

/// The first candidate that is an executable file.
///
/// The error names every path that was tried: "feedback silently vanished" is
/// worse than no feedback loop at all, so a missing binary has to be a sentence
/// the user can act on.
pub fn resolve_ty_binary(candidates: &[PathBuf]) -> Result<PathBuf, IpcError> {
    for candidate in candidates {
        if is_executable(candidate) {
            return Ok(candidate.clone());
        }
    }
    let looked = candidates
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Err(IpcError::internal(format!(
        "could not find the `ty` binary, so nothing was filed. Looked in: {}. \
         Set MACH_TY_BIN to its absolute path.",
        if looked.is_empty() { "nowhere" } else { &looked }
    )))
}

fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        return meta.permissions().mode() & 0o111 != 0;
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// The checkout this build came from.
///
/// `CARGO_MANIFEST_DIR` is baked in at compile time and points at `src-tauri/`,
/// whose parent is the repo — which is exactly right for the dev-mode build
/// this loop exists to serve. `MACH_REPO_ROOT` overrides it.
pub fn repo_root() -> PathBuf {
    if let Ok(explicit) = std::env::var("MACH_REPO_ROOT") {
        if !explicit.trim().is_empty() {
            return PathBuf::from(explicit.trim());
        }
    }
    if let Some(parent) = Path::new(env!("CARGO_MANIFEST_DIR")).parent() {
        if parent.join("package.json").is_file() {
            return parent.to_path_buf();
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let guess = Path::new(&home).join("Projects/mach");
        if guess.is_dir() {
            return guess;
        }
    }
    std::env::temp_dir()
}

/// Where annotated screenshots live. Durable on purpose: an agent opens this
/// file minutes after it is written, long after any temp dir would be swept.
pub fn feedback_dir() -> PathBuf {
    if let Ok(explicit) = std::env::var("MACH_FEEDBACK_DIR") {
        if !explicit.trim().is_empty() {
            return PathBuf::from(explicit.trim());
        }
    }
    repo_root().join(".feedback")
}

/// `feedback-20260807-142233-104.png` — sortable, and correlates with "the
/// thing I was just doing" at a glance.
pub fn screenshot_file_name(now_ms: i64) -> String {
    let stamp = chrono::DateTime::from_timestamp_millis(now_ms)
        .unwrap_or_else(|| chrono::DateTime::from_timestamp_millis(0).expect("epoch is valid"))
        .with_timezone(&chrono::Local);
    format!(
        "feedback-{}-{:03}.png",
        stamp.format("%Y%m%d-%H%M%S"),
        now_ms.rem_euclid(1000)
    )
}

/// The short commit of `repo_root`, read straight out of `.git` — cheap enough
/// to be worth including, and one less subprocess than `git rev-parse`.
pub fn git_commit(repo_root: &Path) -> Option<String> {
    let dot_git = repo_root.join(".git");
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else {
        // A worktree's `.git` is a file: `gitdir: /abs/path`.
        let pointer = std::fs::read_to_string(&dot_git).ok()?;
        PathBuf::from(pointer.trim().strip_prefix("gitdir:")?.trim())
    };

    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    let sha = match head.strip_prefix("ref:") {
        Some(reference) => {
            let reference = reference.trim();
            std::fs::read_to_string(git_dir.join(reference))
                .ok()
                .map(|s| s.trim().to_string())
                // A packed ref has no loose file; the branch name still says
                // more than nothing.
                .unwrap_or_else(|| reference.to_string())
        }
        None => head.to_string(),
    };
    let short: String = sha.chars().take(12).collect();
    (!short.is_empty()).then_some(short)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ===========================================================================
// The commands
// ===========================================================================

/// Capture the app's own window and hand it back as a PNG data URL.
///
/// Returned as `data:image/png;base64,…` rather than a path so the frontend can
/// put it straight into an `<img>` without an asset-protocol grant, and so no
/// file exists on disk until the user actually submits something.
///
/// Nothing here blocks: the subprocess and the file read are both awaited.
#[tauri::command]
pub async fn capture_window(app: tauri::AppHandle) -> Result<String, IpcError> {
    if !cfg!(target_os = "macos") {
        return Err(IpcError::internal(
            "window capture is implemented for macOS only",
        ));
    }

    let rect = main_window_rect(&app);
    let out = std::env::temp_dir().join(format!("mach-capture-{}.png", now_ms()));
    let args = capture_args(rect, &out);

    let output = tokio::process::Command::new(SCREENCAPTURE)
        .args(&args)
        .output()
        .await
        .map_err(|e| IpcError::internal(format!("could not run {SCREENCAPTURE}: {e}")))?;

    if !output.status.success() {
        let _ = tokio::fs::remove_file(&out).await;
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(IpcError::internal(format!(
            "screencapture failed{}{}. macOS may be withholding the Screen Recording \
             permission — grant it to Mach in System Settings › Privacy & Security.",
            output
                .status
                .code()
                .map(|c| format!(" (exit {c})"))
                .unwrap_or_default(),
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            },
        )));
    }

    let bytes = tokio::fs::read(&out)
        .await
        .map_err(|e| IpcError::internal(format!("could not read the capture back: {e}")))?;
    let _ = tokio::fs::remove_file(&out).await;

    if bytes.is_empty() {
        return Err(IpcError::internal("the capture came back empty"));
    }
    Ok(format!("{PNG_DATA_URL}{}", STANDARD.encode(bytes)))
}

/// Write the annotated PNG somewhere durable, then file and queue a TaskYou task.
#[tauri::command]
pub async fn submit_feedback(
    _app: tauri::AppHandle,
    text: String,
    image_png_base64: Option<String>,
    context: Option<FeedbackContext>,
) -> Result<FeedbackReceipt, IpcError> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err(IpcError::internal(
            "say a line about what should change — the screenshot alone is not a request",
        ));
    }

    let root = repo_root();

    // The image is written before anything can fail, so a broken `ty` costs the
    // task but never the capture.
    let screenshot = match image_png_base64
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(data) => {
            let bytes = decode_png(data)?;
            let dir = feedback_dir();
            ensure_feedback_dir(&dir).await?;
            let path = dir.join(screenshot_file_name(now_ms()));
            tokio::fs::write(&path, &bytes).await.map_err(|e| {
                IpcError::internal(format!(
                    "could not write the screenshot to {}: {e}",
                    path.display()
                ))
            })?;
            Some(path)
        }
        None => None,
    };

    let saved = screenshot
        .as_ref()
        .map(|p| format!(" Your annotated screenshot is kept at {}.", p.display()))
        .unwrap_or_default();

    let title = derive_title(&text);
    let body = build_body(&FeedbackReport {
        text: &text,
        screenshot: screenshot.as_deref(),
        context: context.as_ref(),
        repo_root: &root,
        app_version: env!("CARGO_PKG_VERSION"),
        commit: git_commit(&root).as_deref(),
    });

    let binary = resolve_ty_binary(&ty_candidates())
        .map_err(|e| IpcError::internal(format!("{e}{saved}")))?;

    let output = tokio::process::Command::new(&binary)
        .args(ty_args(&title, &body))
        .current_dir(&root)
        .output()
        .await
        .map_err(|e| {
            IpcError::internal(format!("could not run {}: {e}{saved}", binary.display()))
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if !output.status.success() {
        return Err(IpcError::internal(format!(
            "`ty create` failed{}: {}{saved}",
            output
                .status
                .code()
                .map(|c| format!(" (exit {c})"))
                .unwrap_or_default(),
            if stderr.is_empty() { &stdout } else { &stderr },
        )));
    }

    let task_id = parse_task_id(&stdout);
    Ok(FeedbackReceipt {
        message: match task_id {
            Some(id) => format!(
                "Filed as task #{id} in the mach project and queued for immediate execution."
            ),
            None => "Filed in the mach project and queued for immediate execution.".to_string(),
        },
        task_id,
        screenshot_path: screenshot.map(|p| p.display().to_string()),
        output: if stdout.is_empty() { stderr } else { stdout },
    })
}

/// Create `.feedback/` with a `.gitignore` that ignores the whole directory —
/// these are local artifacts and none of them belongs in a commit.
async fn ensure_feedback_dir(dir: &Path) -> Result<(), IpcError> {
    tokio::fs::create_dir_all(dir).await.map_err(|e| {
        IpcError::internal(format!("could not create {}: {e}", dir.display()))
    })?;
    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        let _ = tokio::fs::write(
            &ignore,
            "# Feedback captures are local artifacts of the in-app feedback loop.\n*\n!.gitignore\n",
        )
        .await;
    }
    Ok(())
}

/// The main window's rect in points, top-left origin — what `-R` wants.
fn main_window_rect(app: &tauri::AppHandle) -> Option<WindowRect> {
    let window = app
        .get_webview_window("main")
        .or_else(|| app.webview_windows().into_values().next())?;

    let scale = window.scale_factor().ok()?;
    let position = window.outer_position().ok()?.to_logical::<f64>(scale);
    let size = window.outer_size().ok()?.to_logical::<f64>(scale);

    Some(WindowRect {
        x: position.x.round() as i32,
        y: position.y.round() as i32,
        width: size.width.round().max(0.0) as u32,
        height: size.height.round().max(0.0) as u32,
    })
}
