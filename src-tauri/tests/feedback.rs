//! What the feedback loop *runs*, asserted without running it.
//!
//! `ty create --execute` queues an agent against a real repo, so no test here
//! is allowed to invoke it. Everything that decides the command line is a pure
//! function, and these tests drive those functions directly: the title, the
//! body the receiving agent reads, the argv, the `screencapture` invocation,
//! and the failure path when the binary is not there at all.

use std::path::{Path, PathBuf};

use mach_lib::ipc::feedback::{
    build_body, capture_args, decode_png, derive_title, git_commit, inbox_file_name, parse_task_id,
    resolve_ty_binary, screenshot_file_name, session_item, sink_from_env, ty_args, FeedbackContext,
    FeedbackReport, Sink, WindowRect, TY_PROJECT,
};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn report<'a>(text: &'a str, screenshot: Option<&'a Path>) -> FeedbackReport<'a> {
    FeedbackReport {
        text,
        screenshot,
        context: None,
        repo_root: Path::new("/Users/alex/Projects/mach"),
        app_version: "0.1.0",
        commit: Some("abc123def456"),
    }
}

/// The argument to a named flag, so assertions read as "the body contains …"
/// rather than as an index into a vector.
fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let index = args.iter().position(|a| a == name)?;
    args.get(index + 1).map(String::as_str)
}

// ---------------------------------------------------------------------------
// which sink
// ---------------------------------------------------------------------------

#[test]
fn the_session_is_the_default_sink() {
    assert_eq!(sink_from_env(None), Sink::Session);
    assert_eq!(sink_from_env(Some("")), Sink::Session);
    assert_eq!(sink_from_env(Some("session")), Sink::Session);
}

#[test]
fn taskyou_has_to_be_asked_for_by_name() {
    assert_eq!(sink_from_env(Some("taskyou")), Sink::TaskYou);
    assert_eq!(sink_from_env(Some(" TaskYou ")), Sink::TaskYou);
    assert_eq!(sink_from_env(Some("ty")), Sink::TaskYou);
}

#[test]
fn a_typo_lands_where_somebody_is_looking() {
    // The two sinks fail differently: an item in the inbox nobody watches is
    // still a file the owner can see, and a TaskYou task is a thing he was
    // told about days later. A misspelling should not choose the second.
    assert_eq!(sink_from_env(Some("taskyu")), Sink::Session);
    assert_eq!(sink_from_env(Some("whatever")), Sink::Session);
}

// ---------------------------------------------------------------------------
// the inbox item
// ---------------------------------------------------------------------------

#[test]
fn an_item_is_named_in_arrival_order() {
    let earlier = inbox_file_name(1_754_000_000_000);
    let later = inbox_file_name(1_754_000_060_000);
    assert!(earlier.starts_with("item-"), "{earlier}");
    assert!(earlier.ends_with(".json"), "{earlier}");
    assert!(!earlier.contains(".png"), "{earlier}");
    assert!(earlier < later, "{earlier} should sort before {later}");
}

#[test]
fn an_item_carries_the_same_brief_the_other_sink_would_have_filed() {
    let shot = PathBuf::from("/Users/alex/Projects/mach/.feedback/feedback-1.png");
    let report = report("the reply zone is cramped", Some(&shot));
    let item = session_item(&report, "the reply zone is cramped", 1_754_000_000_000);
    let parsed: serde_json::Value = serde_json::from_str(&item).expect("an item is JSON");

    assert_eq!(parsed["title"], "the reply zone is cramped");
    assert_eq!(parsed["text"], "the reply zone is cramped");
    assert_eq!(parsed["screenshot"], shot.display().to_string());
    assert_eq!(parsed["repoRoot"], "/Users/alex/Projects/mach");
    assert_eq!(parsed["commit"], "abc123def456");
    assert_eq!(parsed["createdAt"], 1_754_000_000_000_i64);
    assert_eq!(parsed["body"], build_body(&report));
}

#[test]
fn an_item_says_what_was_on_screen() {
    let mut report = report("wrong account", None);
    let context = FeedbackContext {
        mode: Some("mail".into()),
        account: Some("bruno@example.com".into()),
        ..Default::default()
    };
    report.context = Some(&context);

    let item = session_item(&report, "wrong account", 1_754_000_000_000);
    let parsed: serde_json::Value = serde_json::from_str(&item).expect("an item is JSON");

    assert_eq!(parsed["context"]["mode"], "mail");
    assert_eq!(parsed["context"]["account"], "bruno@example.com");
    assert!(parsed["screenshot"].is_null(), "no screenshot was taken");
}

// ---------------------------------------------------------------------------
// the command line
// ---------------------------------------------------------------------------

#[test]
fn ty_args_are_a_create_into_the_mach_project_queued_for_execution() {
    let args = ty_args("Row is too cramped", "body text");

    assert_eq!(args[0], "create");
    assert_eq!(args[1], "Row is too cramped", "the title is positional");
    assert_eq!(flag(&args, "--body"), Some("body text"));
    assert_eq!(flag(&args, "--project"), Some(TY_PROJECT));
    assert_eq!(TY_PROJECT, "mach");
    assert!(
        args.iter().any(|a| a == "--execute"),
        "feedback must be queued for immediate execution, not left in a backlog: {args:?}"
    );
}

#[test]
fn nothing_is_shell_quoted_because_nothing_touches_a_shell() {
    // A body full of quotes, newlines and `$` would be a disaster through `sh -c`.
    let body = "he said \"this row\"\nis $too cramped; rm -rf /";
    let args = ty_args("t", body);
    assert_eq!(
        flag(&args, "--body"),
        Some(body),
        "argv entries are passed through verbatim"
    );
}

// ---------------------------------------------------------------------------
// the title
// ---------------------------------------------------------------------------

#[test]
fn title_is_the_first_non_empty_line() {
    assert_eq!(
        derive_title("\n\n  move the account bar to the left  \nand make it thinner"),
        "move the account bar to the left"
    );
}

#[test]
fn title_collapses_whitespace_and_never_ends_mid_word() {
    let long = "the thread row in the unified stream is far too cramped vertically and \
                should breathe a little more than it currently does";
    let title = derive_title(long);

    assert!(title.chars().count() <= 73, "got {} chars", title.chars().count());
    assert!(title.ends_with('…'));
    assert!(
        long.starts_with(title.trim_end_matches('…')),
        "the truncated title must be a prefix of what he typed: {title}"
    );
    assert!(!title.contains("  "));
}

#[test]
fn title_falls_back_rather_than_filing_an_empty_string() {
    assert_eq!(derive_title("   \n\t "), "Feedback from inside Mach");
}

// ---------------------------------------------------------------------------
// the body — what the receiving agent actually reads
// ---------------------------------------------------------------------------

#[test]
fn body_carries_the_text_and_the_absolute_screenshot_path() {
    let path = PathBuf::from("/Users/alex/Projects/mach/.feedback/feedback-20260807-142233-104.png");
    let body = build_body(&report("this row is too cramped", Some(&path)));

    assert!(body.contains("this row is too cramped"));
    assert!(body.contains(path.to_str().unwrap()));
    assert!(
        path.is_absolute(),
        "the agent reads this path from a different cwd, so it must be absolute"
    );
}

#[test]
fn body_names_the_repo_the_version_and_the_commit() {
    let body = build_body(&report("make it blue", None));

    assert!(body.contains("/Users/alex/Projects/mach"));
    assert!(body.contains("0.1.0"));
    assert!(body.contains("abc123def456"));
}

#[test]
fn body_says_so_when_there_is_no_screenshot() {
    let body = build_body(&report("make it blue", None));
    assert!(
        body.to_lowercase().contains("text only"),
        "a missing screenshot must be stated, not silently absent:\n{body}"
    );
}

#[test]
fn body_records_which_view_was_open() {
    let context = FeedbackContext {
        mode: Some("mail".into()),
        view: None,
        label: Some("INBOX".into()),
        account: Some("alex@example.com".into()),
        thread: Some("Re: Series A data room".into()),
    };
    let mut r = report("that one", None);
    r.context = Some(&context);
    let body = build_body(&r);

    assert!(body.contains("Mode: mail"));
    assert!(body.contains("INBOX"));
    assert!(body.contains("alex@example.com"));
    assert!(body.contains("Re: Series A data room"));
    assert!(
        !body.contains("Calendar view:"),
        "absent context is left out, not rendered as 'unknown'"
    );
}

#[test]
fn body_tells_the_agent_which_edits_hot_reload() {
    let body = build_body(&report("nudge it", None));
    assert!(body.contains("src-tauri"));
    assert!(body.to_lowercase().contains("hot-reload"));
}

// ---------------------------------------------------------------------------
// screencapture
// ---------------------------------------------------------------------------

#[test]
fn capture_uses_the_window_rect_when_one_is_known() {
    let args = capture_args(
        Some(WindowRect { x: 120, y: 64, width: 1440, height: 900 }),
        Path::new("/tmp/shot.png"),
    );

    assert!(args.contains(&"-R120,64,1440,900".to_string()), "{args:?}");
    assert!(args.contains(&"-x".to_string()), "no shutter sound");
    assert_eq!(args.last().unwrap(), "/tmp/shot.png");
    assert!(!args.contains(&"-m".to_string()));
}

#[test]
fn capture_falls_back_to_the_main_display_without_a_rect() {
    for rect in [None, Some(WindowRect { x: 0, y: 0, width: 0, height: 900 })] {
        let args = capture_args(rect, Path::new("/tmp/shot.png"));
        assert!(args.contains(&"-m".to_string()), "{args:?}");
        assert!(!args.iter().any(|a| a.starts_with("-R")), "{args:?}");
    }
}

// ---------------------------------------------------------------------------
// failure paths
// ---------------------------------------------------------------------------

#[test]
fn a_missing_ty_binary_names_every_path_it_tried() {
    let candidates = vec![
        PathBuf::from("/nowhere/at/all/ty"),
        PathBuf::from("/also/not/here/ty"),
    ];
    let error = resolve_ty_binary(&candidates).expect_err("nothing there to find");
    let message = error.to_string();

    assert!(message.contains("/nowhere/at/all/ty"), "{message}");
    assert!(message.contains("/also/not/here/ty"), "{message}");
    assert!(
        message.contains("MACH_TY_BIN"),
        "the error has to say how to fix it: {message}"
    );
}

#[test]
fn a_directory_is_not_a_ty_binary() {
    let error = resolve_ty_binary(&[std::env::temp_dir()]);
    assert!(error.is_err(), "a directory must not be run as a program");
}

#[test]
fn an_executable_candidate_wins() {
    // Every unix has this one, and it is unambiguously an executable file.
    let found = resolve_ty_binary(&[
        PathBuf::from("/nowhere/at/all/ty"),
        PathBuf::from("/bin/sh"),
    ])
    .expect("/bin/sh exists");
    assert_eq!(found, PathBuf::from("/bin/sh"));
}

// ---------------------------------------------------------------------------
// odds and ends
// ---------------------------------------------------------------------------

#[test]
fn png_decodes_with_or_without_the_data_url_prefix() {
    // "PNG" — the payload does not have to be a real image to prove the framing.
    let bare = "UE5H";
    assert_eq!(decode_png(bare).unwrap(), b"PNG");
    assert_eq!(
        decode_png(&format!("data:image/png;base64,{bare}")).unwrap(),
        b"PNG"
    );
    assert_eq!(decode_png(&format!("  {bare}\n")).unwrap(), b"PNG");
    assert!(decode_png("").is_err());
    assert!(decode_png("not base64 ~~~").is_err());
}

#[test]
fn task_id_is_read_out_of_ty_output() {
    assert_eq!(parse_task_id("Created task #418: Row is too cramped"), Some(418));
    assert_eq!(parse_task_id("no number here"), None);
    assert_eq!(parse_task_id(""), None);
}

#[test]
fn screenshot_names_are_png_and_sortable() {
    let name = screenshot_file_name(1_754_579_953_104);
    assert!(name.starts_with("feedback-"), "{name}");
    assert!(name.ends_with(".png"), "{name}");
    assert!(name.contains("-104."), "the millisecond is kept: {name}");
    assert_eq!(name.len(), "feedback-20260807-142233-104.png".len(), "{name}");
    assert_ne!(
        screenshot_file_name(1_754_579_953_104),
        screenshot_file_name(1_754_579_953_105),
        "two captures in the same second must not collide"
    );
}

#[test]
fn git_commit_reads_the_checkout_or_gives_up_quietly() {
    assert_eq!(git_commit(Path::new("/definitely/not/a/repo")), None);

    // This test runs inside the repo, so the real answer is available.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    if root.join(".git").exists() {
        let commit = git_commit(root).expect("mach is a git checkout");
        assert!(!commit.is_empty());
        assert!(commit.chars().count() <= 12);
    }
}
