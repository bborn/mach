//! `mach` — Mach's tool surface from a shell.
//!
//! Thin on purpose: everything is in `mach_lib::cli`, where it can be tested
//! without a process. See that module for the design, and for why this is a
//! second binary rather than a subcommand of the app.
//!
//! ```sh
//! mach tools                                  # every verb, generated from the app's own
//! mach search invoice                         # a read: no app required
//! mach archive 4127 --yes                     # a write: over the door, app required
//! mach send_draft d_812 --yes --to a@b.test   # a send: every recipient named
//! ```

fn main() {
    std::process::exit(mach_lib::cli::app::main());
}
