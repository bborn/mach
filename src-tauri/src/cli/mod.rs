//! `mach` — the app's own tool surface, from a shell.
//!
//! # The design, in one sentence
//!
//! There is no command-line vocabulary. The verbs are
//! [`ToolGate::tools`](crate::ipc::agent::engine::gate::ToolGate::tools) — which
//! is [`Command::catalogue`](crate::commands::Command::catalogue) plus the local
//! reads, the composer and the installed plugins — read at runtime, with each
//! tool's `inputSchema` becoming that verb's flags. Anything ⌘K can do, `mach`
//! can do, on the same day, without a line changing here. A hand-written
//! parallel vocabulary would have been a second list to keep in step, and the
//! first thing it would have done is fall behind.
//!
//! # Two routes, and the reason they are different
//!
//! **Reads are direct.** A verb in
//! [`tools::READ_TOOLS`](crate::ipc::agent::engine::tools::READ_TOOLS) is
//! answered inside the CLI process, out of SQLite opened by
//! [`Db::open_read_only`](crate::db::Db::open_read_only). WAL is what makes this
//! free: a second reader never blocks the app and is never blocked by it. So
//! `mach search invoice` works whether or not Mach is open, costs nothing, and
//! runs the *same* query the agent's `search_threads` runs — see
//! [`tools::execute_read`](crate::ipc::agent::engine::tools::execute_read),
//! which exists so there is one implementation rather than two that can come to
//! disagree about what a match is.
//!
//! **Writes go over loopback to the running app, or they fail.** Only that
//! process holds the OAuth tokens, the outbox's ten-second recall timer and the
//! undo stack. A second writer would break the one-writer invariant `db`'s
//! module doc makes a type-level fact — and, worse than the lock, a local write
//! that Google never accepted has no revert path outside the app, while
//! `users.history.list` only ever reports changes that *happened*. Such a write
//! would be invisible to incremental sync and would survive until a full
//! resync. So when the app is not running a write **fails loudly and says so**.
//! It does not queue and it does not half-succeed. Driving this app from outside
//! has already destroyed live mail once; the tempting version of this feature is
//! the one that would do it again quietly.
//!
//! # Layout
//!
//! | module | what lives there |
//! |---|---|
//! | [`door`] | the loopback listener the app opens, and why it may live for hours |
//! | [`protocol`] | the consent rule, as pure functions over data |
//! | [`endpoint`] | the `0600` file that says where the door is |
//! | [`client`] | the CLI's half: find the door, knock, read the answer |
//! | [`args`] | a command line, turned into a tool call by the tool's own schema |
//! | [`render`] | a tool outcome, for a terminal |
//! | [`error`] | `{ kind, message }`, and the exit code it becomes |
//! | [`app`] | the program |
//!
//! Only [`door`] runs inside the application. Everything else is either shared
//! or is the CLI's, which is what lets `tests/cli.rs` drive the consent rule and
//! the argument parser with no app anywhere near them.
//!
//! # A second binary, not a subcommand
//!
//! `mach-cli` is its own `[[bin]]`, beside the three development probes already
//! in `src/bin/`. The alternative — a branch at the top of `main.rs` that
//! declines to call `mach_lib::run()` when argv has a verb in it — was rejected
//! for three reasons, in increasing order of importance.
//!
//! The shallow one: a macOS GUI binary lives at
//! `Mach.app/Contents/MacOS/mach`, which is not a path anybody has on `$PATH`,
//! and putting it there means symlinking into a bundle that the updater
//! replaces wholesale.
//!
//! The practical one: that binary is linked against the webview and brings up
//! an `NSApplication`. Every `mach search` would pay for a GUI process it then
//! declines to become, in a tool a script calls in a loop.
//!
//! The one that decided it: **a separate binary cannot open a window and cannot
//! take the writer.** The two-route rule above is the whole safety argument of
//! this feature, and in a subcommand it would be a branch — one `if` away from
//! a CLI invocation that booted the app far enough to hold `Db::write`, at which
//! point there are two writers on one store and the invariant is a comment. Here
//! it is a fact about which executable is running. The read path in this crate
//! has no dispatcher to reach Google with and no writable connection to reach
//! SQLite with, because it never constructs either.
//!
//! What it costs is that the binary is `mach-cli` and the command people want to
//! type is `mach`. `scripts/mach` is the shim during development; a release
//! installs the artifact under whatever name the operator prefers. That is a
//! naming problem, and naming problems are cheaper than concurrency ones.

/// The program.
pub mod app;
/// A command line, turned into a tool call by the tool's own schema.
pub mod args;
/// The CLI's half of the conversation with the door.
pub mod client;
/// The loopback listener the app opens for the command line.
pub mod door;
/// Where the door is, and the `0600` file that says so.
pub mod endpoint;
/// One error shape, and the exit codes it turns into.
pub mod error;
/// The consent rule.
pub mod protocol;
/// A tool outcome, for a terminal.
pub mod render;

pub use error::CliError;
