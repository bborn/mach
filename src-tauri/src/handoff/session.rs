//! A handoff that stays in the window: one process on a pty, and the pane it
//! talks to.
//!
//! Terminal mode hands a `.command` file to Terminal.app and lets go. Inline
//! mode runs something that returns and shows what it printed. Neither of those
//! is `claude "{{prompt}}"`, which is the command the owner actually configured:
//! an interactive session that wants a terminal, wants keystrokes, and does not
//! return. This is the third answer — the same [`LaunchPlan`], run on a
//! pseudo-terminal Mach owns, with a pane in the window as its screen.
//!
//! # Several at a time
//!
//! [`Sessions`] holds a list, capped at [`MAX_SESSIONS`]. The pane is a tab
//! strip: each session has its own pty, its own scrollback, its own resolved
//! prompt and its own reaping, and the ceiling is refused by number rather than
//! by silently backgrounding anything. The reaping guarantees below are stated
//! per session and hold for all of them at once — [`Sessions::close_all`] reaps
//! every one, and the third guarantee never involved Mach running code anyway.
//!
//! # What a session was given
//!
//! A session can be handed things it must not outlive — the tool server in
//! [`crate::ipc::agent::engine::mcp`] is one, and its bearer token is why. Those arrive as
//! [`SessionResource`]s, are held in [`Live`], and are dropped by [`reap`] after
//! the process is dead. There is no other way to release one, which is what
//! makes "the token dies with the pane" a property of the type rather than of
//! remembering to call something.
//!
//! # How the process dies
//!
//! A leaked `claude` holding a pty is the failure mode worth engineering
//! against, so there are three independent guarantees and the last of them does
//! not involve Mach running any code at all:
//!
//! 1. **The pane closed.** [`Sessions::close`] signals the child's *process
//!    group* — not just the child — with `SIGHUP`, then `SIGKILL` if it is
//!    still there [`KILL_GRACE`] later. The group is the unit because a session
//!    leader that spawns workers is the normal case, and `claude` does.
//! 2. **The app quit.** `lib.rs` calls [`Sessions::close_all`] on
//!    `RunEvent::Exit`, which is the same path.
//! 3. **The app crashed, or was `kill -9`d.** Nothing runs, and the process
//!    still dies: the master file descriptor is closed by the kernel with
//!    everything else Mach had open, the pty's slave side sees the hangup, and
//!    the foreground process group gets `SIGHUP` from the tty driver. This is
//!    the only guarantee that holds when the code above never executes, which
//!    is why the master is never duplicated anywhere it could outlive a session.
//!
//! # How a flood is survived
//!
//! `yes` writes about a gigabyte a second, and every byte of it would otherwise
//! become a Tauri event, a JSON string, and a webview repaint. Two threads and
//! one cap stop that:
//!
//! * the **reader** blocks on the pty and appends to a buffer, which is capped
//!   at [`MAX_PENDING_BYTES`]. Past the cap the *oldest* bytes go, because the
//!   newest are the ones still on screen, and the count of what was dropped
//!   travels with the next chunk so the pane can say so;
//! * the **flusher** wakes every [`FLUSH_INTERVAL`] and emits at most this
//!   session's *share* of the budget, once.
//!
//! The share is the second half, and it is what stops four tabs being four
//! floods. The budget is [`MAX_PENDING_BYTES`] per frame for the window, not per
//! session, so a session's share is that divided by however many are live: one
//! session may emit 64 KiB per frame, four may emit 16 KiB each, and the number
//! of bytes the webview is asked to decode and paint in a frame is the same
//! either way. What a share does not cover stays in the buffer, ages, and is
//! dropped from the front by the cap above — which is the same rule as before:
//! the newest bytes are the ones on screen.
//!
//! Backpressure does the rest: a process that outruns the reader fills the
//! kernel's pty buffer and blocks in `write`, which is exactly what happens to
//! `yes` in a real terminal that is not being read fast enough.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize};

use super::plan::LaunchPlan;
use super::HandoffError;

/// The pane's size before it has measured itself. Only ever used for the
/// instant between spawn and the first resize.
pub const DEFAULT_COLS: u16 = 100;
pub const DEFAULT_ROWS: u16 = 30;

/// A pane narrower or shorter than this is a rendering bug, not a request.
pub const MIN_COLS: u16 = 2;
pub const MIN_ROWS: u16 = 1;
/// And one larger than this is a number that arrived from the webview wrong.
pub const MAX_COLS: u16 = 1000;
pub const MAX_ROWS: u16 = 400;

/// One frame. The pane is redrawn at most this often however loud the process.
pub const FLUSH_INTERVAL: Duration = Duration::from_millis(16);

/// The most output that can be waiting for the next flush, per session, and the
/// whole window's budget for one frame. Past this the oldest bytes are dropped —
/// see the module doc.
pub const MAX_PENDING_BYTES: usize = 64 * 1024;

/// How many sessions the pane will hold.
///
/// Four, because that is how many tabs fit across the strip at a readable width
/// and because every one of them is a pty, a reader thread, a flusher thread and
/// a share of one frame's budget. A fifth is refused by number; nothing is
/// closed to make room for it.
pub const MAX_SESSIONS: usize = 4;

/// The most one keystroke or paste may carry into the pty.
pub const MAX_WRITE_BYTES: usize = 1024 * 1024;

/// How long a `SIGHUP`ped process group has before `SIGKILL`.
pub const KILL_GRACE: Duration = Duration::from_millis(750);

/// Where the pane's output and its ending arrive.
///
/// A trait rather than an `AppHandle` so the tests can run a real process on a
/// real pty and read what it wrote without an application around them.
pub trait SessionSink: Send + Sync + 'static {
    /// A chunk of the process's output, and how many bytes were dropped in
    /// front of it because the pane could not keep up.
    fn output(&self, session_id: &str, bytes: Vec<u8>, dropped: u64);
    /// The process is gone. Always the last call for a session, and always
    /// after every `output` that preceded it.
    fn exited(&self, session_id: &str, status: Option<i32>);
}

/// Something a session was given that must not outlive it.
///
/// The one implementation that matters holds the MCP listener and the file its
/// bearer token is written to; dropping it closes the port and deletes the file.
/// Held in [`Live`] and dropped by [`reap`], so the only way to release it is the
/// same three-guarantee path the process itself dies on.
pub trait SessionResource: Send + Sync {
    /// One word for the pane's header. Never more than a label.
    fn label(&self) -> String;
}

/// What starting one costs the caller to know about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Started {
    pub session_id: String,
    pub target_name: String,
    /// argv joined for display. Nothing runs this string.
    pub command: String,
    pub dir: String,
    /// The prompt the process was started with, exactly as it was passed.
    pub prompt: String,
    pub context_file: String,
    /// What this session was handed of Mach itself, by label. Empty for a
    /// target that was given nothing.
    pub resources: Vec<String>,
}

/// The buffer between the reader and the flusher.
#[derive(Default)]
struct Pending {
    bytes: Vec<u8>,
    dropped: u64,
    /// The reader has seen EOF; the flusher drains and stops.
    done: bool,
    status: Option<i32>,
}

/// One running session.
struct Live {
    /// What the pane was told when this started, kept so that a webview which
    /// reloaded — hot module replacement in development, a crashed renderer in
    /// production — can find the process again instead of leaving it running
    /// with nothing on screen pointing at it.
    started: Started,
    id: String,
    /// Kept so the pane can be resized, and — more importantly — so that
    /// dropping this struct closes the master and hangs the process up.
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    /// The child's own pid, which is also its process group id: `portable-pty`
    /// calls `setsid` before `exec`, so the child leads both.
    pid: Option<u32>,
    stop: Arc<AtomicBool>,
    /// What this session was given. Dropped by [`reap`], after the process is
    /// gone, and never anywhere else.
    resources: Vec<Box<dyn SessionResource>>,
}

/// Every session there is, and the operations on them.
#[derive(Default)]
pub struct Sessions {
    live: Mutex<Vec<Live>>,
    /// How many are running, read by every flusher to work out its share of one
    /// frame. An atomic rather than a lock because it is read on a timer by one
    /// thread per session and written twice per session lifetime.
    count: Arc<AtomicUsize>,
}

impl Sessions {
    pub fn new() -> Sessions {
        Sessions::default()
    }

    /// Spawn the plan on a pty and start reading it.
    ///
    /// The plan is the same one terminal and inline mode use, so argv, the
    /// working directory, the environment and the prompt are resolved in
    /// exactly one place for all three modes.
    ///
    /// `resources` is whatever this session was given that must die with it. It
    /// is taken by value and never handed back, so a caller cannot hold a copy.
    pub fn open<S: SessionSink>(
        &self,
        plan: &LaunchPlan,
        cols: u16,
        rows: u16,
        sink: Arc<S>,
        resources: Vec<Box<dyn SessionResource>>,
    ) -> Result<Started, HandoffError> {
        let mut slot = self.live.lock().unwrap_or_else(|e| e.into_inner());
        if slot.len() >= MAX_SESSIONS {
            return Err(HandoffError::Io(format!(
                "{MAX_SESSIONS} sessions are open, which is as many as the pane holds — \
                 close one before starting another"
            )));
        }

        let size = PtySize {
            cols: clamp(cols, MIN_COLS, MAX_COLS, DEFAULT_COLS),
            rows: clamp(rows, MIN_ROWS, MAX_ROWS, DEFAULT_ROWS),
            pixel_width: 0,
            pixel_height: 0,
        };

        let pty = portable_pty::native_pty_system();
        let pair = pty
            .openpty(size)
            .map_err(|e| HandoffError::Io(format!("could not open a pseudo-terminal: {e}")))?;

        let mut command = CommandBuilder::new(&plan.argv[0]);
        command.args(&plan.argv[1..]);
        command.cwd(&plan.dir);
        for (key, value) in &plan.env {
            command.env(key, value);
        }
        // A GUI process inherits launchd's PATH, which does not have `claude`
        // on it. `plan::environment` already worked out what to prepend; this
        // is the same substitution `run_inline` makes, and it has to happen
        // before the spawn because `CommandBuilder` resolves argv[0] against
        // this PATH.
        command.env("PATH", prepended_path(plan.env.get("MACH_HANDOFF_PATH")));
        // What the process is allowed to assume about its screen. The pane is
        // xterm.js, which is what these names describe.
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        command.env("MACH_HANDOFF_MODE", "session");

        let child = pair.slave.spawn_command(command).map_err(|e| {
            HandoffError::Io(format!("could not start {}: {e}", plan.argv[0]))
        })?;
        // The slave descriptor belongs to the child now. Holding a copy would
        // mean the pty never reports EOF when the child exits, and the pane
        // would sit there looking alive forever.
        drop(pair.slave);

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| HandoffError::Io(format!("could not read the pseudo-terminal: {e}")))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| HandoffError::Io(format!("could not write the pseudo-terminal: {e}")))?;

        let id = super::new_tag();
        let pid = child.process_id();
        let killer = child.clone_killer();
        let stop = Arc::new(AtomicBool::new(false));
        let pending = Arc::new(Mutex::new(Pending::default()));

        spawn_reader(reader, child, Arc::clone(&pending));
        spawn_flusher(
            id.clone(),
            Arc::clone(&pending),
            Arc::clone(&stop),
            Arc::clone(&sink),
            Arc::clone(&self.count),
        );

        let started = Started {
            session_id: id.clone(),
            target_name: plan.target_name.clone(),
            command: plan.display_command(),
            dir: plan.dir.to_string_lossy().into_owned(),
            prompt: plan.prompt.clone(),
            context_file: plan.context_file.to_string_lossy().into_owned(),
            resources: resources.iter().map(|r| r.label()).collect(),
        };

        slot.push(Live {
            started: started.clone(),
            id,
            master: pair.master,
            writer,
            killer,
            pid,
            stop,
            resources,
        });
        self.count.store(slot.len(), Ordering::Relaxed);

        Ok(started)
    }

    /// Every session that is running, in the order they were started.
    ///
    /// For a webview that has just loaded and does not know what it is looking
    /// at. Push, not poll: this is asked once on mount, exactly as
    /// `agent_sessions` is, and everything after it arrives on the event.
    pub fn list(&self) -> Vec<Started> {
        let slot = self.live.lock().unwrap_or_else(|e| e.into_inner());
        slot.iter().map(|live| live.started.clone()).collect()
    }

    /// How many are running.
    pub fn len(&self) -> usize {
        self.live.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Keystrokes, or a paste. Bytes, verbatim, with no interpretation.
    pub fn write(&self, session_id: &str, data: &[u8]) -> Result<(), HandoffError> {
        if data.len() > MAX_WRITE_BYTES {
            return Err(HandoffError::Io(format!(
                "that is {} bytes; a session takes {MAX_WRITE_BYTES} at a time",
                data.len()
            )));
        }
        let mut slot = self.live.lock().unwrap_or_else(|e| e.into_inner());
        let live = slot
            .iter_mut()
            .find(|live| live.id == session_id)
            .ok_or_else(no_session)?;
        live.writer
            .write_all(data)
            .and_then(|()| live.writer.flush())
            .map_err(|e| HandoffError::Io(format!("the session would not take that: {e}")))
    }

    /// The pane changed shape. Full-screen programs render wrong without this,
    /// and the `SIGWINCH` that the ioctl produces is how they find out.
    pub fn resize(&self, session_id: &str, cols: u16, rows: u16) -> Result<(), HandoffError> {
        let slot = self.live.lock().unwrap_or_else(|e| e.into_inner());
        let live = slot
            .iter()
            .find(|live| live.id == session_id)
            .ok_or_else(no_session)?;
        live.master
            .resize(PtySize {
                cols: clamp(cols, MIN_COLS, MAX_COLS, DEFAULT_COLS),
                rows: clamp(rows, MIN_ROWS, MAX_ROWS, DEFAULT_ROWS),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| HandoffError::Io(format!("could not resize the session: {e}")))
    }

    /// Whether this id names a session that is running.
    pub fn is_live(&self, session_id: &str) -> bool {
        let slot = self.live.lock().unwrap_or_else(|e| e.into_inner());
        slot.iter().any(|live| live.id == session_id)
    }

    /// End one. Idempotent, and safe to call for a session that has already
    /// exited on its own. The others carry on.
    pub fn close(&self, session_id: &str) {
        let taken = {
            let mut slot = self.live.lock().unwrap_or_else(|e| e.into_inner());
            let taken = slot
                .iter()
                .position(|live| live.id == session_id)
                .map(|index| slot.remove(index));
            self.count.store(slot.len(), Ordering::Relaxed);
            taken
        };
        if let Some(live) = taken {
            reap(live);
        }
    }

    /// Every one of them, for the app going away. See guarantee 2 in the module
    /// doc — it is the same path, run once per session.
    pub fn close_all(&self) {
        let taken = {
            let mut slot = self.live.lock().unwrap_or_else(|e| e.into_inner());
            let taken = std::mem::take(&mut *slot);
            self.count.store(0, Ordering::Relaxed);
            taken
        };
        for live in taken {
            reap(live);
        }
    }
}

fn no_session() -> HandoffError {
    HandoffError::Io("that session is no longer running".into())
}

/// `SIGHUP` the group, close the pty, `SIGKILL` anything still there, and only
/// then let go of what the session was holding.
fn reap(live: Live) {
    let Live {
        started: _,
        id: _,
        master,
        writer,
        mut killer,
        pid,
        stop,
        resources,
    } = live;

    // The flusher stops emitting for a pane that is gone. Doing this first
    // means nothing arrives at a webview that has already dropped the session.
    stop.store(true, Ordering::Relaxed);

    // The group before the child, because a session leader's workers are
    // exactly what "a leaked process holding a pty" is made of.
    signal_group(pid, libc::SIGHUP);
    let _ = killer.kill();

    // Closing the master hangs up the tty, which is the same signal from the
    // other direction and the one that works when nothing above did.
    drop(writer);
    drop(master);

    if let Some(pid) = pid {
        let deadline = Instant::now() + KILL_GRACE;
        loop {
            if !group_exists(pid) {
                break;
            }
            if Instant::now() >= deadline {
                signal_group(Some(pid), libc::SIGKILL);
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    // Last, and only here. A token that could still be used by something the
    // pty was holding open would not have died with the pane; by this line
    // there is nothing left in the process group to use it.
    drop(resources);
}

/// `kill(-pid, signal)` — the process group the child leads.
///
/// `portable-pty` calls `setsid` before `exec`, so the child is a session and
/// process-group leader and its own descendants are in that group unless they
/// leave it deliberately.
fn signal_group(pid: Option<u32>, signal: i32) {
    let Some(pid) = pid else { return };
    // Signalling group 0 or 1 would mean "this process group" and "everything",
    // and neither is ever a pid we spawned.
    if pid <= 1 {
        return;
    }
    unsafe {
        libc::kill(-(pid as i32), signal);
    }
}

/// Whether anything is left in the group. `kill(pid, 0)` asks without sending.
fn group_exists(pid: u32) -> bool {
    if pid <= 1 {
        return false;
    }
    unsafe { libc::kill(-(pid as i32), 0) == 0 }
}

/// Read until the pty hangs up, then reap the child and record its status.
///
/// The child is *owned* here rather than in [`Live`] so that the `wait` happens
/// on this thread the instant EOF arrives — a child that is waited for by
/// nobody is a zombie, and a zombie holding a pty looks exactly like the leak
/// this module exists to prevent.
fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    mut child: Box<dyn Child + Send + Sync>,
    pending: Arc<Mutex<Pending>>,
) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut slot = pending.lock().unwrap_or_else(|e| e.into_inner());
                    slot.bytes.extend_from_slice(&buf[..n]);
                    if slot.bytes.len() > MAX_PENDING_BYTES {
                        let excess = slot.bytes.len() - MAX_PENDING_BYTES;
                        slot.bytes.drain(..excess);
                        slot.dropped += excess as u64;
                    }
                }
            }
        }

        let status = child.wait().ok().map(|status| status.exit_code() as i32);
        let mut slot = pending.lock().unwrap_or_else(|e| e.into_inner());
        slot.status = status;
        slot.done = true;
    });
}

/// Emit at most this session's share of one frame, and the ending after the last
/// of them.
///
/// `live` is how many sessions are running, which is what the share is divided
/// by. It is read every tick rather than captured, so closing three tabs gives
/// the fourth the whole budget back without anything being rebuilt.
fn spawn_flusher<S: SessionSink>(
    id: String,
    pending: Arc<Mutex<Pending>>,
    stop: Arc<AtomicBool>,
    sink: Arc<S>,
    live: Arc<AtomicUsize>,
) {
    std::thread::spawn(move || loop {
        std::thread::sleep(FLUSH_INTERVAL);
        if stop.load(Ordering::Relaxed) {
            return;
        }

        let share = share_of_budget(live.load(Ordering::Relaxed));
        let (bytes, dropped, drained, status) = {
            let mut slot = pending.lock().unwrap_or_else(|e| e.into_inner());
            let take = slot.bytes.len().min(share);
            let bytes: Vec<u8> = slot.bytes.drain(..take).collect();
            (
                bytes,
                std::mem::take(&mut slot.dropped),
                slot.done && slot.bytes.is_empty(),
                slot.status,
            )
        };

        if !bytes.is_empty() || dropped > 0 {
            sink.output(&id, bytes, dropped);
        }
        if drained {
            // The reader sets `done` after its last append, and this drains the
            // buffer before reporting it, so the pane has seen everything the
            // process wrote before it is told the process is gone.
            sink.exited(&id, status);
            return;
        }
    });
}

/// One session's slice of one frame. Never zero, or a lone session with a full
/// buffer would never drain.
fn share_of_budget(live: usize) -> usize {
    MAX_PENDING_BYTES / live.max(1)
}

fn clamp(value: u16, min: u16, max: u16, fallback: u16) -> u16 {
    if value == 0 {
        return fallback;
    }
    value.clamp(min, max)
}

/// The inherited `PATH` with the handoff's extra directories in front.
fn prepended_path(extra: Option<&String>) -> String {
    let current = std::env::var("PATH").unwrap_or_default();
    match extra {
        Some(extra) if !extra.is_empty() && !current.is_empty() => format!("{extra}:{current}"),
        Some(extra) if !extra.is_empty() => extra.clone(),
        _ => current,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_of_zero_is_the_pane_not_having_measured_itself() {
        assert_eq!(clamp(0, MIN_COLS, MAX_COLS, DEFAULT_COLS), DEFAULT_COLS);
        assert_eq!(clamp(1, MIN_COLS, MAX_COLS, DEFAULT_COLS), MIN_COLS);
        assert_eq!(clamp(9999, MIN_COLS, MAX_COLS, DEFAULT_COLS), MAX_COLS);
        assert_eq!(clamp(80, MIN_COLS, MAX_COLS, DEFAULT_COLS), 80);
    }

    #[test]
    fn the_frame_budget_is_the_windows_rather_than_each_sessions() {
        // Four tabs must not be four floods: whatever is live, the bytes handed
        // to the webview in one frame add up to the same number.
        assert_eq!(share_of_budget(0), MAX_PENDING_BYTES);
        assert_eq!(share_of_budget(1), MAX_PENDING_BYTES);
        assert_eq!(share_of_budget(4) * 4, MAX_PENDING_BYTES);
        assert!(share_of_budget(MAX_SESSIONS) > 0);
    }

    #[test]
    fn nothing_is_ever_signalled_at_the_root_of_the_process_tree() {
        // `kill(-1, …)` is "every process this user owns". A pid of 0 or 1 can
        // only mean something went wrong, and the guard is cheap.
        signal_group(None, libc::SIGKILL);
        signal_group(Some(0), libc::SIGKILL);
        signal_group(Some(1), libc::SIGKILL);
        assert!(!group_exists(1));
    }
}
