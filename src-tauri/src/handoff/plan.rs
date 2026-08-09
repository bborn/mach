//! From a target and a context to a running process — argv, environment, files.
//!
//! # Nothing here builds a command line out of an email
//!
//! Inline mode is the easy case: [`LaunchPlan::argv`] is already a vector,
//! `tokio::process::Command` takes it as argv, and no shell is involved at any
//! point. There is nothing to escape because there is nothing that parses.
//!
//! Terminal mode is the case worth reading carefully, because a terminal
//! emulator will only accept a *command line*. Terminal.app has no API that
//! takes an argument vector; it runs a file, and that file is a shell script.
//! So the question is not "can we avoid a shell" — it is "can we make sure the
//! shell never sees a byte of email".
//!
//! It can, and the arrangement is three files:
//!
//! ```text
//!   /tmp/mach-handoff-<tag>/
//!     ├── context.txt      the whole prompt, untruncated  ({{context_file}})
//!     ├── argv.bin         NUL-separated: env assignments, then argv
//!     └── launch.command   #!/bin/sh, four lines, one interpolated path
//! ```
//!
//! `launch.command` contains exactly one value that varies, and it is the path
//! of `argv.bin` — a name Mach generated out of hex. It reads:
//!
//! ```sh
//! exec /usr/bin/xargs -0 /usr/bin/env < '<argv.bin>'
//! ```
//!
//! `xargs -0` splits the file on NUL and passes the pieces to `env` as
//! *arguments*. No shell parses them: they go from bytes in a file to `argv[]`
//! in one `execve`. `env` applies the leading `NAME=value` records to the
//! environment and execs the rest. Between the environment and the program sits
//! one fixed `sh -c` whose script is a compile-time constant — it exists to
//! `cd` into the target's directory, which `env` on macOS cannot do — and the
//! program's own arguments reach it as `"$@"`, which the shell expands without
//! re-parsing. A body containing `"; rm -rf ~; echo "` is one positional
//! parameter with a semicolon in it, in every one of those steps.
//!
//! The directory is not interpolated into the script either. It travels as
//! `MACH_HANDOFF_DIR` in `argv.bin` and is used as `cd "$MACH_HANDOFF_DIR"` —
//! quoted, so no word splitting and no globbing. That is belt and braces: the
//! directory is his own configuration rather than anybody's email. But it costs
//! one line, and it means the only text a shell parses in the whole mechanism is
//! a constant and a temp path.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::context::{HandoffContext, MAX_INLINE_CONTEXT_BYTES};
use super::target::{HandoffMode, HandoffTarget};
use super::template::{self, Values};
use super::HandoffError;

/// How long an inline command may take before Mach stops waiting.
///
/// Inline is for commands that *return* — `ty task create …` prints a line and
/// exits. A target configured inline by mistake, pointed at something that
/// wants to work for ten minutes, must not leave a dialog hanging with no way
/// out; it gets killed and says so, and the fix is one field in the editor.
pub const INLINE_TIMEOUT: Duration = Duration::from_secs(25);

/// Captured output past this is cut. Nobody reads more, and the receipt is a
/// glance, not a log.
pub const MAX_CAPTURED_OUTPUT: usize = 8 * 1024;

/// The fixed script `env` execs, so that the command starts in the right
/// directory with the right `PATH`.
///
/// A compile-time constant, never formatted, never interpolated. The two values
/// it reads come out of the environment and the program's arguments arrive as
/// `"$@"`, which is expanded but not re-parsed — so nothing in here can be
/// turned into shell syntax by anything Mach put in the environment or in argv.
const CD_AND_EXEC: &str = concat!(
    "cd \"$MACH_HANDOFF_DIR\" || exit 1\n",
    "[ -n \"$MACH_HANDOFF_PATH\" ] && PATH=\"$MACH_HANDOFF_PATH:$PATH\" && export PATH\n",
    "exec \"$@\"\n",
);

/// Everything needed to start one handoff, and nothing that has started yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchPlan {
    pub mode: HandoffMode,
    pub target_name: String,
    /// The program and its arguments. Never joined into a string to be run.
    pub argv: Vec<String>,
    /// Sorted so a snapshot in a test means something.
    pub env: BTreeMap<String, String>,
    pub dir: PathBuf,
    /// Where the whole prompt was written. Always the untruncated text.
    pub context_file: PathBuf,
    /// The prompt as it appears in argv — capped at
    /// [`MAX_INLINE_CONTEXT_BYTES`].
    pub prompt: String,
    /// The prompt as written to [`Self::context_file`].
    pub full_prompt: String,
    /// One line naming what the context is, for the confirmation sheet.
    pub context_label: String,
    /// The per-handoff temp directory holding the files above.
    pub work_dir: PathBuf,
}

/// What a launch is willing to say afterwards. Deliberately thin: Mach does not
/// follow a handoff, so this describes the *throw*, never the outcome.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Launched {
    pub target_name: String,
    pub mode: String,
    pub dir: String,
    /// argv, joined for display only. Nothing runs this string.
    pub command: String,
    pub context_file: String,
    /// One sentence for the UI.
    pub message: String,
    /// Inline only: the exit status, and what it printed.
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

// ===========================================================================
// Composition — every decision, no side effects
// ===========================================================================

/// The prompt: his sentence, then the fenced context.
///
/// His instruction goes first because that is the one instruction in the
/// message; see `context`'s module doc for the rest of the reasoning. When the
/// context is too long for argv the *quoted region* is cut and a line points at
/// the file — cut inside the fence, so the closing marker still arrives and the
/// receiving agent never sees an unterminated block.
pub fn compose_prompt(note: &str, context: &HandoffContext, context_file: &Path) -> (String, String) {
    let note = note.trim();
    if context.is_empty() {
        return (note.to_string(), note.to_string());
    }

    let full = format!("{note}\n\n{}\n", context.block);
    if full.len() <= MAX_INLINE_CONTEXT_BYTES {
        return (full.clone(), full);
    }

    // The last line is the closing marker; keep it whatever happens to the rest.
    let (head, marker) = context.block.rsplit_once('\n').unwrap_or((&context.block, ""));
    let notice = format!(
        "\n\n[Cut here by Mach: this thread is longer than fits in one argument.\n\
         The whole of it, untruncated, is in {}.]\n",
        context_file.display()
    );
    let budget = MAX_INLINE_CONTEXT_BYTES
        .saturating_sub(note.len() + notice.len() + marker.len() + 8);
    let cut = floor_char_boundary(head, budget);
    let capped = format!("{note}\n\n{}{notice}{marker}\n", &head[..cut]);
    (capped, full)
}

/// Every `{{name}}` a template can use.
pub fn values(note: &str, context: &HandoffContext, prompt: &str, context_file: &Path) -> Values {
    let mut values = Values::new();
    values.insert("prompt".into(), prompt.to_string());
    values.insert("note".into(), note.trim().to_string());
    values.insert("subject".into(), context.subject.clone());
    values.insert("from".into(), context.from.clone());
    values.insert("date".into(), context.date.clone());
    values.insert("body".into(), context.body.clone());
    values.insert("permalink".into(), context.permalink.clone());
    values.insert("attachments".into(), context.attachments.clone());
    values.insert(
        "context_file".into(),
        context_file.to_string_lossy().into_owned(),
    );
    values
}

/// The same values, as environment variables.
///
/// A second route in, for a `run` that would rather read `$MACH_HANDOFF_BODY`
/// than take a positional argument — and the route that costs nothing, since
/// the environment is copied by `execve` and never parsed by anything. Values
/// are capped for the same reason argv is: the environment shares the kernel's
/// argument budget with it.
pub fn environment(values: &Values, dir: &Path) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for name in template::PLACEHOLDERS {
        if let Some(value) = values.get(*name) {
            let key = format!("MACH_HANDOFF_{}", name.to_uppercase());
            env.insert(key, cap(value, MAX_INLINE_CONTEXT_BYTES));
        }
    }
    env.insert(
        "MACH_HANDOFF_DIR".to_string(),
        dir.to_string_lossy().into_owned(),
    );
    env.insert("MACH_HANDOFF_PATH".to_string(), extra_path());
    // So a receiving tool can tell a handoff from a hand-typed invocation.
    env.insert("MACH_HANDOFF".to_string(), "1".to_string());
    env
}

/// Directories a GUI-launched process would otherwise never see.
///
/// An app started from the Dock inherits `launchd`'s `PATH`, which is
/// `/usr/bin:/bin:/usr/sbin:/sbin` — so `claude` being on `PATH` in a terminal
/// proves nothing about whether inline mode can find it. These are prepended
/// rather than substituted, so a terminal session keeps its own `PATH` and
/// gains these.
fn extra_path() -> String {
    if let Ok(explicit) = std::env::var("MACH_HANDOFF_PATH") {
        if !explicit.trim().is_empty() {
            return explicit;
        }
    }
    let mut dirs: Vec<String> = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        for suffix in [".local/bin", ".bun/bin", ".cargo/bin", "bin"] {
            dirs.push(format!("{home}/{suffix}"));
        }
    }
    dirs.push("/opt/homebrew/bin".to_string());
    dirs.push("/usr/local/bin".to_string());
    dirs.join(":")
}

fn cap(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_string();
    }
    let cut = floor_char_boundary(value, limit.saturating_sub(3));
    format!("{}…", &value[..cut])
}

/// The largest char boundary at or below `index`.
///
/// Every limit in this module is a byte count, and a byte count lands wherever
/// it lands. `str::floor_char_boundary` is still unstable; `render::quotes`
/// carries the same four lines for the same reason, and learned to the hard way.
fn floor_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    let mut i = index;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

// ===========================================================================
// The plan
// ===========================================================================

impl LaunchPlan {
    /// Turn a target and a context into something runnable.
    ///
    /// Writes two files — the context and the argv — and creates nothing else.
    /// `tag` names the working directory, so it must be hex-ish; it comes from
    /// the same value the fence markers carry.
    pub fn prepare(
        target: &HandoffTarget,
        note: &str,
        context: &HandoffContext,
        tag: &str,
    ) -> Result<LaunchPlan, HandoffError> {
        super::target::validate(target)?;
        if note.trim().is_empty() {
            return Err(HandoffError::NothingToSay(
                "say what you want done — a handoff is your sentence, not just the mail".into(),
            ));
        }

        let dir = PathBuf::from(expand_home(target.dir.trim()));
        if !dir.is_dir() {
            return Err(HandoffError::Io(format!(
                "{} is not a directory — nothing was launched",
                dir.display()
            )));
        }

        let work_dir = work_dir_for(tag);
        std::fs::create_dir_all(&work_dir)
            .map_err(|e| HandoffError::Io(format!("could not create {}: {e}", work_dir.display())))?;
        let context_file = work_dir.join("context.txt");

        let (prompt, full_prompt) = compose_prompt(note, context, &context_file);
        std::fs::write(&context_file, full_prompt.as_bytes()).map_err(|e| {
            HandoffError::Io(format!("could not write {}: {e}", context_file.display()))
        })?;

        let values = values(note, context, &prompt, &context_file);
        let tokens = template::tokenize(&target.run)?;
        // An empty argument cannot survive the NUL-separated file the terminal
        // path reads — `xargs` treats two adjacent separators as one — so it is
        // dropped on *both* paths rather than being silently dropped on one.
        // Every template that produces one is a placeholder that resolved to
        // nothing, which means nothing either way.
        let argv: Vec<String> = template::substitute(&tokens, &values)
            .into_iter()
            .filter(|token| !token.is_empty())
            .collect();
        if argv.is_empty() {
            return Err(HandoffError::BadTemplate(
                "the run template produced no command".into(),
            ));
        }

        Ok(LaunchPlan {
            mode: target.mode,
            target_name: target.name.clone(),
            env: environment(&values, &dir),
            argv,
            dir,
            context_file,
            prompt,
            full_prompt,
            context_label: context.label.clone(),
            work_dir,
        })
    }

    /// argv joined for a human to read. Never executed, never parsed.
    pub fn display_command(&self) -> String {
        self.argv
            .iter()
            .map(|arg| {
                let one_line = arg.replace('\n', "⏎");
                if one_line.contains(' ') {
                    format!("\"{}\"", one_line.replace('"', "\\\""))
                } else {
                    one_line
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The NUL-separated file `xargs -0` reads: assignments first, then the
    /// fixed `sh -c` shim, then the program and its arguments.
    pub fn argv_file_bytes(&self) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::with_capacity(4096);
        let mut push = |record: &str| {
            out.extend_from_slice(record.as_bytes());
            out.push(0);
        };
        for (key, value) in &self.env {
            push(&format!("{key}={value}"));
        }
        push("/bin/sh");
        push("-c");
        push(CD_AND_EXEC);
        // `$0` for the shim. Names the process in `ps` and keeps the real argv0
        // in `$@` where `exec "$@"` expects it.
        push("mach-handoff");
        for arg in &self.argv {
            push(arg);
        }
        out
    }

    /// The whole `.command` file Terminal will run.
    ///
    /// One interpolated value, and it is a path this process made out of hex.
    pub fn launcher_script(&self, argv_file: &Path) -> String {
        format!(
            "#!/bin/sh\n\
             # Written by Mach for one handoff, then forgotten about.\n\
             #\n\
             # Nothing from an email appears in this file. The command and its\n\
             # arguments live in the file below as NUL-separated records; xargs\n\
             # hands them to env as arguments, so no shell ever parses them.\n\
             exec /usr/bin/xargs -0 /usr/bin/env < '{}'\n",
            argv_file.display()
        )
    }
}

/// `~/x` in a configured directory. The one place a target's `dir` is expanded.
fn expand_home(dir: &str) -> String {
    let Some(rest) = dir.strip_prefix("~/") else {
        return dir.to_string();
    };
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => format!("{}/{rest}", home.trim_end_matches('/')),
        _ => dir.to_string(),
    }
}

/// Per-handoff scratch. Left behind on purpose: `context.txt` is the thing
/// `{{context_file}}` points at, and the agent reads it minutes later.
pub fn work_dir_for(tag: &str) -> PathBuf {
    let safe: String = tag
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(32)
        .collect();
    std::env::temp_dir().join(format!("mach-handoff-{safe}"))
}

// ===========================================================================
// Launching
// ===========================================================================

/// Run it here, wait for it, and keep what it printed.
pub async fn run_inline(plan: &LaunchPlan) -> Result<Launched, HandoffError> {
    let mut command = tokio::process::Command::new(&plan.argv[0]);
    command
        .args(&plan.argv[1..])
        .current_dir(&plan.dir)
        .envs(&plan.env)
        .env("PATH", prepended_path(plan.env.get("MACH_HANDOFF_PATH")))
        .stdin(std::process::Stdio::null());

    let output = tokio::time::timeout(inline_timeout(), command.output())
        .await
        .map_err(|_| {
            HandoffError::Io(format!(
                "{} was still running after {} seconds and was left behind. \
                 Inline mode is for commands that return; use terminal mode for a session.",
                plan.argv[0],
                inline_timeout().as_secs()
            ))
        })?
        .map_err(|e| HandoffError::Io(format!("could not run {}: {e}", plan.argv[0])))?;

    let stdout = cap(String::from_utf8_lossy(&output.stdout).trim(), MAX_CAPTURED_OUTPUT);
    let stderr = cap(String::from_utf8_lossy(&output.stderr).trim(), MAX_CAPTURED_OUTPUT);
    let status = output.status.code();

    Ok(Launched {
        message: match status {
            Some(0) | None => format!("Handed to {}.", plan.target_name),
            Some(code) => format!("{} exited {code}.", plan.target_name),
        },
        target_name: plan.target_name.clone(),
        mode: plan.mode.as_str().to_string(),
        dir: plan.dir.to_string_lossy().into_owned(),
        command: plan.display_command(),
        context_file: plan.context_file.to_string_lossy().into_owned(),
        status,
        stdout,
        stderr,
    })
}

/// Write the three files and let the terminal have it.
pub fn open_in_terminal(plan: &LaunchPlan) -> Result<Launched, HandoffError> {
    let argv_file = plan.work_dir.join("argv.bin");
    std::fs::write(&argv_file, plan.argv_file_bytes())
        .map_err(|e| HandoffError::Io(format!("could not write {}: {e}", argv_file.display())))?;

    let script = plan.work_dir.join("launch.command");
    std::fs::write(&script, plan.launcher_script(&argv_file))
        .map_err(|e| HandoffError::Io(format!("could not write {}: {e}", script.display())))?;
    make_executable(&script)?;

    let open = std::process::Command::new("/usr/bin/open")
        .args(open_args(&script))
        .status()
        .map_err(|e| HandoffError::Io(format!("could not run /usr/bin/open: {e}")))?;

    if !open.success() {
        return Err(HandoffError::Io(format!(
            "the terminal would not open{}. The command is ready at {}.",
            open.code().map(|c| format!(" (exit {c})")).unwrap_or_default(),
            script.display()
        )));
    }

    Ok(Launched {
        message: format!("Handed to {} in a terminal.", plan.target_name),
        target_name: plan.target_name.clone(),
        mode: plan.mode.as_str().to_string(),
        dir: plan.dir.to_string_lossy().into_owned(),
        command: plan.display_command(),
        context_file: plan.context_file.to_string_lossy().into_owned(),
        status: None,
        stdout: String::new(),
        stderr: String::new(),
    })
}

/// `open <script>`, or `open -a <app> <script>` when he named one.
///
/// The bare form goes to whatever handles `.command`, which is Terminal unless
/// he has told macOS otherwise — and if he has, that is the answer he wants.
pub fn open_args(script: &Path) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    if let Ok(app) = std::env::var("MACH_HANDOFF_TERMINAL_APP") {
        if !app.trim().is_empty() {
            args.push("-a".to_string());
            args.push(app.trim().to_string());
        }
    }
    args.push(script.to_string_lossy().into_owned());
    args
}

fn make_executable(path: &Path) -> Result<(), HandoffError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| HandoffError::Io(format!("could not chmod {}: {e}", path.display())))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn prepended_path(extra: Option<&String>) -> String {
    let current = std::env::var("PATH").unwrap_or_default();
    match extra {
        Some(extra) if !extra.is_empty() && !current.is_empty() => format!("{extra}:{current}"),
        Some(extra) if !extra.is_empty() => extra.clone(),
        _ => current,
    }
}

fn inline_timeout() -> Duration {
    std::env::var("MACH_HANDOFF_INLINE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map(Duration::from_secs)
        .unwrap_or(INLINE_TIMEOUT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_launcher_script_holds_nothing_but_a_generated_path() {
        let plan = LaunchPlan {
            mode: HandoffMode::Terminal,
            target_name: "T".into(),
            argv: vec!["claude".into(), "\"; rm -rf ~; echo \"".into()],
            env: BTreeMap::new(),
            dir: PathBuf::from("/tmp"),
            context_file: PathBuf::from("/tmp/x/context.txt"),
            prompt: String::new(),
            full_prompt: String::new(),
            context_label: String::new(),
            work_dir: PathBuf::from("/tmp/x"),
        };
        let script = plan.launcher_script(Path::new("/tmp/x/argv.bin"));
        assert!(!script.contains("rm -rf"), "argv must not appear in the script");
        assert!(script.contains("/tmp/x/argv.bin"));
    }
}
