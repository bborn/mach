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
//!   [`several_floods_share_one_windows_worth_of_budget`] is the same claim with
//!   the tab strip full, which is the one that would otherwise multiply.
//!
//! And [`what_a_session_was_given_dies_with_it`] is the property the tool server
//! rests on: a resource handed to a session is dropped by the same three paths
//! the process dies on, and by nothing else.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mach_lib::ipc::handoff::engine::context::HandoffSource;
use mach_lib::ipc::handoff::engine::plan::LaunchPlan;
use mach_lib::ipc::handoff::engine::session::{
    SessionResource, SessionSink, Sessions, MAX_PENDING_BYTES, MAX_SESSIONS,
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

    /// Forget everything so far. For a measurement that must start after the
    /// thing being measured has settled.
    fn clear(&self) {
        self.chunks.lock().unwrap().clear();
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
    let id = start(&sessions, run, note, Arc::clone(&sink));
    (sessions, sink, id)
}

/// Another session in an existing registry — a second tab.
fn start(sessions: &Sessions, run: &str, note: &str, sink: Arc<Recorder>) -> String {
    sessions
        .open(&plan(run, note), 80, 24, sink, Vec::new())
        .expect("the session must start")
        .session_id
}

/// Something a session was given. Says so when it is dropped.
struct Held(Arc<AtomicUsize>);

impl SessionResource for Held {
    fn label(&self) -> String {
        "Mach's tools".to_string()
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
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

#[test]
fn several_floods_share_one_windows_worth_of_budget() {
    // The whole reason a tab strip is not four times the flood. Three `yes`
    // processes, all shouting; the number of bytes the webview is handed per
    // frame is the window's budget, divided, not multiplied.
    let sessions = Sessions::new();
    let sinks: Vec<Arc<Recorder>> = (0..3).map(|_| Arc::new(Recorder::default())).collect();
    let ids: Vec<String> = sinks
        .iter()
        .map(|sink| {
            start(
                &sessions,
                r#"/usr/bin/yes 0123456789abcdefghijklmnopqrstuvwxyz"#,
                "flood",
                Arc::clone(sink),
            )
        })
        .collect();

    // The share is read per tick, so a session that is alone for the first
    // millisecond of its life legitimately gets the whole budget for it. The
    // claim is about three sessions running, so the measurement starts once
    // three are.
    std::thread::sleep(Duration::from_millis(150));
    for sink in &sinks {
        sink.clear();
    }

    std::thread::sleep(Duration::from_millis(400));

    // Read before closing, for the same reason: closing the first two puts the
    // third back on the whole budget, which is correct and is not what is being
    // measured.
    let measured: Vec<Vec<usize>> = sinks
        .iter()
        .map(|sink| {
            let chunks = sink.chunks.lock().unwrap();
            chunks.iter().map(|(bytes, _)| bytes.len()).collect()
        })
        .collect();
    for id in &ids {
        sessions.close(id);
    }

    let mut total = 0usize;
    for sizes in &measured {
        assert!(!sizes.is_empty(), "each session must produce something");
        for size in sizes {
            assert!(
                *size <= MAX_PENDING_BYTES / 3,
                "a chunk of {size} bytes is more than this session's share of a frame"
            );
        }
        total += sizes.iter().sum::<usize>();
    }

    // 400ms is 25 frames; three sessions at 64 KiB per frame between them is
    // about 1.6 MB. Doubled for a loaded machine — the assertion that matters
    // is that this is not three times the single-session number, which `yes`
    // would otherwise make hundreds of megabytes.
    assert!(
        total <= 4 * 1024 * 1024,
        "{total} bytes in 400ms across three sessions is a firehose aimed at the webview"
    );
}

// ---------------------------------------------------------------------------
// Several at a time
// ---------------------------------------------------------------------------

#[test]
fn the_ceiling_is_refused_by_number_and_freed_by_closing_one() {
    let sessions = Sessions::new();
    let mut ids = Vec::new();
    for _ in 0..MAX_SESSIONS {
        ids.push(start(
            &sessions,
            r#"/bin/sh -c "sleep 30""#,
            "hold a tab",
            Arc::new(Recorder::default()),
        ));
    }
    assert_eq!(sessions.list().len(), MAX_SESSIONS);

    let past = sessions.open(
        &plan(r#"/bin/sh -c "sleep 30""#, "one too many"),
        80,
        24,
        Arc::new(Recorder::default()),
        Vec::new(),
    );
    let message = past.expect_err("the ceiling must refuse").to_string();
    assert!(
        message.contains(&MAX_SESSIONS.to_string()) && message.contains("close one"),
        "the refusal must say how many and what to do: {message}"
    );

    // Nothing was closed to make room, and closing one makes room.
    assert_eq!(sessions.list().len(), MAX_SESSIONS);
    sessions.close(&ids[0]);
    assert!(sessions
        .open(
            &plan(r#"/bin/echo done"#, "after"),
            80,
            24,
            Arc::new(Recorder::default()),
            Vec::new(),
        )
        .is_ok());

    sessions.close_all();
}

#[test]
fn each_session_has_its_own_pty_and_closing_one_leaves_the_others() {
    let sessions = Sessions::new();
    let first = Arc::new(Recorder::default());
    let second = Arc::new(Recorder::default());
    let a = start(
        &sessions,
        r#"/bin/sh -c "while read line; do echo a=$line; done""#,
        "first",
        Arc::clone(&first),
    );
    let b = start(
        &sessions,
        r#"/bin/sh -c "while read line; do echo b=$line; done""#,
        "second",
        Arc::clone(&second),
    );

    sessions.write(&a, b"one\n").expect("write to the first");
    first.wait_for("a=one", Duration::from_secs(5));
    assert!(first.text().contains("a=one"));
    assert!(
        !second.text().contains("one"),
        "a keystroke must reach the tab it was typed in and no other"
    );

    sessions.write(&b, b"two\n").expect("write to the second");
    second.wait_for("b=two", Duration::from_secs(5));
    assert!(second.text().contains("b=two"));

    // Closing one leaves the other running and writable.
    sessions.close(&a);
    assert!(!sessions.is_live(&a));
    assert!(sessions.is_live(&b), "closing one tab must not close the rest");
    sessions.write(&b, b"three\n").expect("the survivor still takes keystrokes");
    second.wait_for("b=three", Duration::from_secs(5));
    assert!(second.text().contains("b=three"));

    sessions.close_all();
}

#[test]
fn the_app_going_away_reaps_every_session_at_once() {
    let sessions = Sessions::new();
    let mut pids = Vec::new();
    for _ in 0..3 {
        let sink = Arc::new(Recorder::default());
        start(
            &sessions,
            r#"/bin/sh -c "sleep 120 & echo leader=$$; wait""#,
            "outlive me",
            Arc::clone(&sink),
        );
        let text = sink.wait_for("leader=", Duration::from_secs(5));
        pids.push(
            text.split("leader=")
                .nth(1)
                .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).find(|s| !s.is_empty()))
                .and_then(|digits| digits.parse::<i32>().ok())
                .unwrap_or_else(|| panic!("no pid in {text:?}")),
        );
    }

    sessions.close_all();
    for pid in pids {
        assert!(
            wait_until_gone(pid, Duration::from_secs(5)),
            "the app going away must take every session's process group with it"
        );
    }
    assert!(sessions.is_empty());
}

#[test]
fn writing_to_a_session_that_is_not_running_is_refused() {
    let (sessions, _sink, id) = open(r#"/bin/sh -c "sleep 30""#, "hold a tab");

    assert!(sessions.is_live(&id));
    assert!(!sessions.is_live("not-a-session"));
    assert!(sessions.write("not-a-session", b"x").is_err());
    assert!(sessions.resize("not-a-session", 80, 24).is_err());
    // Closing an id that is not live must not close one that is.
    sessions.close("not-a-session");
    assert!(sessions.is_live(&id));

    sessions.close(&id);
}

// ---------------------------------------------------------------------------
// What a session was given
// ---------------------------------------------------------------------------

#[test]
fn what_a_session_was_given_dies_with_it() {
    // The tool server's bearer token is one of these. It must not be releasable
    // by anything except the paths the process itself dies on, and it must go
    // for every session when the app quits.
    let dropped = Arc::new(AtomicUsize::new(0));
    let sessions = Sessions::new();

    let started = sessions
        .open(
            &plan(r#"/bin/sh -c "sleep 30""#, "hold a tab"),
            80,
            24,
            Arc::new(Recorder::default()),
            vec![Box::new(Held(Arc::clone(&dropped)))],
        )
        .expect("the session must start");

    // On the wire, so the tab can say what this session can reach.
    assert_eq!(started.resources, vec!["Mach's tools".to_string()]);
    assert_eq!(sessions.list()[0].resources, started.resources);
    assert_eq!(dropped.load(Ordering::SeqCst), 0, "still running, still held");

    sessions.close(&started.session_id);
    assert_eq!(
        dropped.load(Ordering::SeqCst),
        1,
        "closing the pane must release what the session was given"
    );

    // And the other path: the app quitting, with the pane never closed.
    let second = sessions
        .open(
            &plan(r#"/bin/sh -c "sleep 30""#, "and another"),
            80,
            24,
            Arc::new(Recorder::default()),
            vec![Box::new(Held(Arc::clone(&dropped)))],
        )
        .expect("the session must start");
    assert!(!second.resources.is_empty());
    sessions.close_all();
    assert_eq!(
        dropped.load(Ordering::SeqCst),
        2,
        "the app going away must release it too"
    );
}

#[test]
fn a_session_that_was_given_nothing_says_so() {
    let (sessions, _sink, id) = open(r#"/bin/sh -c "sleep 30""#, "no tools here");
    assert!(
        sessions.list()[0].resources.is_empty(),
        "a target Mach gives nothing to must report nothing"
    );
    sessions.close(&id);
}
