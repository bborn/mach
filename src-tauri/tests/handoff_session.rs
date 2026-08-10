//! Tests for the session pane's engine (`handoff::session`).
//!
//! Every one of these runs a real process on a real pseudo-terminal. Nothing
//! here spawns `claude`: the assertions are about the pty, the plan that
//! reaches it, and the corpse afterwards, so the programs are `/bin/sh`,
//! `/bin/echo` and `/usr/bin/yes` — installed everywhere, deterministic, and
//! offline.
//!
//! The two that would be expensive to learn the hard way:
//!
//! * [`closing_the_pane_kills_the_whole_process_group`] starts a child *and* a
//!   background grandchild, closes the pane, and then asks the kernel whether
//!   either is still there. A leaked `claude` holding a pty is the failure this
//!   feature could most easily ship with, and it would be invisible from inside
//!   the app.
//! * [`a_flood_of_output_stays_bounded`] runs `yes` and counts what came out
//!   the other end. The pane is a webview; a process that can write a gigabyte
//!   a second must not be able to send a gigabyte a second of Tauri events.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mach_lib::ipc::handoff::engine::context::HandoffSource;
use mach_lib::ipc::handoff::engine::plan::LaunchPlan;
use mach_lib::ipc::handoff::engine::session::{
    SessionSink, Sessions, MAX_PENDING_BYTES,
};
use mach_lib::ipc::handoff::engine::target::{HandoffMode, HandoffTarget};
use mach_lib::ipc::handoff::engine::{context, new_tag};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Everything a session said, in order.
#[derive(Default)]
struct Recorder {
    chunks: Mutex<Vec<(Vec<u8>, u64)>>,
    exit: Mutex<Option<Option<i32>>>,
}

impl SessionSink for Recorder {
    fn output(&self, _session_id: &str, bytes: Vec<u8>, dropped: u64) {
        self.chunks.lock().unwrap().push((bytes, dropped));
    }

    fn exited(&self, _session_id: &str, status: Option<i32>) {
        *self.exit.lock().unwrap() = Some(status);
    }
}

impl Recorder {
    fn text(&self) -> String {
        let chunks = self.chunks.lock().unwrap();
        let mut out: Vec<u8> = Vec::new();
        for (bytes, _) in chunks.iter() {
            out.extend_from_slice(bytes);
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    fn dropped(&self) -> u64 {
        self.chunks.lock().unwrap().iter().map(|(_, d)| d).sum()
    }

    fn exited(&self) -> Option<Option<i32>> {
        *self.exit.lock().unwrap()
    }

    /// Spin until the output contains `needle`, or give up. Sessions are
    /// threads and a pty is a kernel buffer, so nothing here can assert
    /// immediately after acting.
    fn wait_for(&self, needle: &str, within: Duration) -> String {
        let deadline = Instant::now() + within;
        loop {
            let text = self.text();
            if text.contains(needle) || Instant::now() > deadline {
                return text;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for_exit(&self, within: Duration) -> Option<Option<i32>> {
        let deadline = Instant::now() + within;
        loop {
            if let Some(status) = self.exited() {
                return Some(status);
            }
            if Instant::now() > deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn target(run: &str) -> HandoffTarget {
    HandoffTarget {
        id: "t1".into(),
        name: "Session".into(),
        dir: std::env::temp_dir().to_string_lossy().into_owned(),
        run: run.into(),
        mode: HandoffMode::Session,
        last_run_at: None,
    }
}

/// A plan with no mail behind it, so `prompt` is exactly the note and an
/// assertion about what the process received is an assertion about one line.
fn plan(run: &str, note: &str) -> LaunchPlan {
    let tag = new_tag();
    let context = context::build(&HandoffSource::None, &tag);
    LaunchPlan::prepare(&target(run), note, &context, &tag).expect("plan")
}

fn open(run: &str, note: &str) -> (Sessions, Arc<Recorder>, String) {
    let sessions = Sessions::new();
    let sink = Arc::new(Recorder::default());
    let plan = plan(run, note);
    let started = sessions
        .open(&plan, 80, 24, Arc::clone(&sink))
        .expect("the session must start");
    (sessions, sink, started.session_id)
}

/// Whether any process is left in the group `pid` leads.
fn group_alive(pid: i32) -> bool {
    unsafe { libc::kill(-pid, 0) == 0 }
}

fn wait_until_gone(pid: i32, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if !group_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    !group_alive(pid)
}

// ---------------------------------------------------------------------------
// The plan reaches the process
// ---------------------------------------------------------------------------

#[test]
fn the_pty_spawns_with_the_targets_resolved_argv_cwd_and_environment() {
    // `$0` under `sh -c` is the argument after the script, which is where the
    // template put `{{prompt}}` — so this prints the resolved prompt, the
    // working directory the target named, and one of the variables
    // `plan::environment` sets, each on its own line.
    let (sessions, sink, id) = open(
        r#"/bin/sh -c "pwd; echo mode=$MACH_HANDOFF; echo prompt=$0" {{prompt}}"#,
        "reschedule the standups",
    );

    let text = sink.wait_for("prompt=", Duration::from_secs(5));
    let dir = std::fs::canonicalize(std::env::temp_dir()).expect("canonical temp dir");

    assert!(
        text.contains(dir.to_string_lossy().trim_end_matches('/')),
        "the process must start in the target's directory, got: {text:?}"
    );
    assert!(text.contains("mode=1"), "the handoff environment must travel: {text:?}");
    assert!(
        text.contains("prompt=reschedule the standups"),
        "the resolved prompt must arrive as one argument: {text:?}"
    );

    sessions.close(&id);
}

#[test]
fn a_hostile_prompt_is_still_one_argument_on_a_pty() {
    // The same claim `tests/handoff.rs` makes about the other two modes. A pty
    // is a screen, not a shell: nothing here re-parses what was substituted.
    let payload = "\"; touch /tmp/mach-session-pwned; echo \"";
    let (sessions, sink, id) = open(r#"/bin/echo {{prompt}}"#, payload);

    let text = sink.wait_for("touch", Duration::from_secs(5));
    assert!(text.contains(payload), "the payload must arrive verbatim: {text:?}");
    assert!(
        !std::path::Path::new("/tmp/mach-session-pwned").exists(),
        "the payload must not have been executed"
    );

    sessions.close(&id);
}

#[test]
fn keystrokes_reach_the_process() {
    let (sessions, sink, id) = open(r#"/bin/sh -c "read line; echo got=$line""#, "type at it");

    sessions.write(&id, b"hello\n").expect("write");
    let text = sink.wait_for("got=hello", Duration::from_secs(5));
    assert!(text.contains("got=hello"), "what was typed must arrive: {text:?}");

    sessions.close(&id);
}

#[test]
fn a_resize_reaches_the_process() {
    // `stty size` reads the kernel's idea of the window, which is what the
    // resize ioctl sets and what makes a full-screen program render correctly.
    let (sessions, sink, id) = open(
        r#"/bin/sh -c "stty size; read x; stty size""#,
        "measure yourself",
    );

    sink.wait_for("24 80", Duration::from_secs(5));
    sessions.resize(&id, 132, 43).expect("resize");
    // Give the ioctl a moment to land before the second reading is taken.
    std::thread::sleep(Duration::from_millis(100));
    sessions.write(&id, b"\n").expect("write");

    let text = sink.wait_for("43 132", Duration::from_secs(5));
    assert!(
        text.contains("43 132"),
        "the process must see the pane's new size: {text:?}"
    );

    sessions.close(&id);
}

#[test]
fn a_session_that_ends_on_its_own_reports_its_status_after_its_last_output() {
    let (sessions, sink, id) = open(r#"/bin/sh -c "echo finished; exit 3""#, "run and stop");

    let status = sink.wait_for_exit(Duration::from_secs(5));
    assert_eq!(status, Some(Some(3)), "the exit code must reach the pane");
    assert!(
        sink.text().contains("finished"),
        "everything written before the exit must have been delivered first"
    );
    // Closing a session that has already ended is the ordinary case.
    sessions.close(&id);
}

// ---------------------------------------------------------------------------
// Reaping
// ---------------------------------------------------------------------------

#[test]
fn closing_the_pane_kills_the_whole_process_group() {
    // A background grandchild is what `claude` looks like from out here.
    let (sessions, sink, id) = open(
        r#"/bin/sh -c "sleep 120 & echo leader=$$; wait""#,
        "start something long",
    );

    let text = sink.wait_for("leader=", Duration::from_secs(5));
    let pid: i32 = text
        .split("leader=")
        .nth(1)
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).find(|s| !s.is_empty()))
        .and_then(|digits| digits.parse().ok())
        .unwrap_or_else(|| panic!("no pid in {text:?}"));

    assert!(group_alive(pid), "the group must be running before it is closed");
    sessions.close(&id);
    assert!(
        wait_until_gone(pid, Duration::from_secs(5)),
        "nothing may be left in the process group after the pane closes"
    );
}

#[test]
fn closing_everything_reaps_a_session_the_pane_never_closed() {
    // `close_all` is what the app's exit runs, and it must not need the id.
    let (sessions, sink, _id) = open(r#"/bin/sh -c "echo leader=$$; sleep 120""#, "outlive me");

    let text = sink.wait_for("leader=", Duration::from_secs(5));
    let pid: i32 = text
        .split("leader=")
        .nth(1)
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).find(|s| !s.is_empty()))
        .and_then(|digits| digits.parse().ok())
        .unwrap_or_else(|| panic!("no pid in {text:?}"));

    sessions.close_all();
    assert!(
        wait_until_gone(pid, Duration::from_secs(5)),
        "the app going away must take the session with it"
    );
}

#[test]
fn a_process_that_ignores_the_hangup_is_killed_anyway() {
    // `trap "" HUP` is a process refusing the polite request. The escalation to
    // SIGKILL is what makes "the process dies with the pane" a guarantee rather
    // than a convention.
    let (sessions, sink, id) = open(
        r#"/bin/sh -c "trap '' HUP; echo leader=$$; while :; do sleep 1; done""#,
        "refuse to leave",
    );

    let text = sink.wait_for("leader=", Duration::from_secs(5));
    let pid: i32 = text
        .split("leader=")
        .nth(1)
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).find(|s| !s.is_empty()))
        .and_then(|digits| digits.parse().ok())
        .unwrap_or_else(|| panic!("no pid in {text:?}"));

    sessions.close(&id);
    assert!(
        wait_until_gone(pid, Duration::from_secs(5)),
        "a process that ignores SIGHUP must still be gone"
    );
}

// ---------------------------------------------------------------------------
// Bounds
// ---------------------------------------------------------------------------

#[test]
fn a_flood_of_output_stays_bounded() {
    let (sessions, sink, id) = open(r#"/usr/bin/yes 0123456789abcdefghijklmnopqrstuvwxyz"#, "flood");

    std::thread::sleep(Duration::from_millis(400));
    sessions.close(&id);

    let sizes: Vec<usize> = {
        let chunks = sink.chunks.lock().unwrap();
        chunks.iter().map(|(bytes, _)| bytes.len()).collect()
    };

    assert!(!sizes.is_empty(), "the flood must produce something");
    for size in &sizes {
        assert!(
            *size <= MAX_PENDING_BYTES,
            "a chunk of {size} bytes would be handed to the webview whole"
        );
    }
    // One chunk per frame, plus slack for a loaded machine. `yes` writes
    // hundreds of megabytes in 400ms; without the cap this would be thousands.
    assert!(
        sizes.len() <= 60,
        "{} events in 400ms is a firehose aimed at the webview",
        sizes.len()
    );
    assert!(
        sink.dropped() > 0,
        "output the pane could not keep up with must be reported as dropped, not queued"
    );
}

#[test]
fn a_paste_larger_than_the_limit_is_refused_rather_than_forwarded() {
    let (sessions, _sink, id) = open(r#"/bin/sh -c "cat > /dev/null""#, "take a paste");

    let huge = vec![b'x'; 2 * 1024 * 1024];
    let refused = sessions.write(&id, &huge);
    assert!(refused.is_err(), "a two-megabyte keystroke is not a keystroke");

    sessions.close(&id);
}

// ---------------------------------------------------------------------------
// One at a time
// ---------------------------------------------------------------------------

#[test]
fn a_second_session_is_refused_by_name() {
    let (sessions, _sink, id) = open(r#"/bin/sh -c "sleep 30""#, "hold the pane");

    let second = sessions.open(
        &plan(r#"/bin/sh -c "sleep 30""#, "and another"),
        80,
        24,
        Arc::new(Recorder::default()),
    );
    let message = second.expect_err("a second session must be refused").to_string();
    assert!(
        message.contains("Session") && message.contains("already running"),
        "the refusal must name what is in the way: {message}"
    );

    sessions.close(&id);
    // And the pane is free again once it is closed.
    let third = sessions.open(
        &plan(r#"/bin/echo done"#, "after"),
        80,
        24,
        Arc::new(Recorder::default()),
    );
    assert!(third.is_ok(), "closing must free the pane");
    sessions.close_all();
}

#[test]
fn writing_to_a_session_that_is_not_the_live_one_is_refused() {
    let (sessions, _sink, id) = open(r#"/bin/sh -c "sleep 30""#, "hold the pane");

    assert!(sessions.is_live(&id));
    assert!(!sessions.is_live("not-a-session"));
    assert!(sessions.write("not-a-session", b"x").is_err());
    assert!(sessions.resize("not-a-session", 80, 24).is_err());
    // Closing an id that is not the live one must not close the live one.
    sessions.close("not-a-session");
    assert!(sessions.is_live(&id));

    sessions.close(&id);
}
