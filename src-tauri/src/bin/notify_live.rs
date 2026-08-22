//! Put one real banner on the screen, and say what clicking it opened.
//!
//! A banner is drawn by macOS, so no test and no screenshot of the app can show
//! you whether it looks right or whether clicking it lands. This is the one way
//! to look at it, and it runs the same code the sync loop does: [`rule::digest`]
//! composes it and [`notify::mac::deliver`] delivers it.
//!
//! ```sh
//! cargo run --bin notify_live            # one message: sender / subject / preview
//! cargo run --bin notify_live -- digest   # the coalesced shape
//! ```
//!
//! It then blocks until the banner is clicked or cleared, and prints the
//! conversation the click routed to — which is the assertion a screenshot
//! cannot make.
//!
//! Nothing here touches a store, an account, or the network: the arrivals are
//! made up on the spot. It is deliberately a separate binary rather than
//! anything the app can reach, because the one thing a notification must never
//! be is something a process other than the owner's own Mach can fire.

// Every item below is gated, which is heavier than gating the two calls that
// happen not to compile elsewhere — `notify::mac::deliver` and `CFRunLoopRun`
// — and is the honest shape: the probe *is* macOS. Its whole job is to put an
// `NSUserNotification` on the screen and block until a hand clicks it, and
// there is nothing on the other side to port to. The banner Mach posts on
// Linux is drawn and routed by the desktop's own notification daemon, which
// reports the activation back through the plugin, so the thing this binary
// exists to see cannot go unseen there. Without the gate `cargo test` — which
// builds every target, this one included — fails on Linux before a single test
// runs.
#[cfg(target_os = "macos")]
use std::sync::{Arc, Mutex};

#[cfg(target_os = "macos")]
use mach_lib::db::Db;
#[cfg(target_os = "macos")]
use mach_lib::notify::{
    self,
    host::{Delivery, Host},
    rule::{self, Arrival},
    Banner, PendingOpen, Permission,
};

/// The identifier in `tauri.conf.json`. Repeated rather than read, because this
/// binary has no Tauri config loaded and one string is cheaper than a parser.
/// `scripts/dev-bundle` and `scripts/sign` hold the same one.
#[cfg(target_os = "macos")]
const IDENTIFIER: &str = "com.mach.mail";

/// A host that only records, so the click can be printed rather than emitted
/// into a window this process does not have.
#[cfg(target_os = "macos")]
struct Printer {
    opened: Mutex<Vec<PendingOpen>>,
}

#[cfg(target_os = "macos")]
impl Host for Printer {
    fn show(&self, _banner: &Banner, _target: &PendingOpen) -> Delivery {
        Delivery::Watched
    }
    fn set_badge(&self, _count: Option<i64>) {}
    fn reopen(&self) {
        println!("→ reopen(): the window would come forward");
    }
    fn open_conversation(&self, target: &PendingOpen) {
        println!(
            "clicked → open_conversation(): thread_id={} gmail_thread_id={}",
            target.thread_id, target.gmail_thread_id
        );
        self.opened.lock().unwrap().push(target.clone());
        // The run loop below has nothing else to do once this has been said.
        std::process::exit(0);
    }
    fn db(&self) -> Option<Db> {
        None
    }
    fn permission(&self) -> Permission {
        Permission::Granted
    }
    fn request_permission(&self) -> Permission {
        Permission::Granted
    }
}

#[cfg(target_os = "macos")]
fn arrival(id: &str, thread_id: i64, sender: &str, subject: &str, snippet: &str) -> Arrival {
    Arrival {
        gmail_message_id: id.into(),
        thread_id,
        gmail_thread_id: format!("t-{id}"),
        from_name: Some(sender.into()),
        from_email: format!("{}@example.com", sender.to_lowercase().replace(' ', ".")),
        subject: subject.into(),
        snippet: snippet.into(),
        labels: vec![rule::INBOX.into(), rule::UNREAD.into()],
        thread_has_own_message: false,
    }
}

#[cfg(target_os = "macos")]
fn main() {
    let coalesced = std::env::args().nth(1).as_deref() == Some("digest");

    let arrivals = if coalesced {
        vec![
            arrival("m1", 101, "Anna Lee", "Lunch?", "Are you free Thursday?"),
            arrival("m2", 102, "Bob Chen", "Re: invoice", "Attached, finally."),
            arrival(
                "m3",
                103,
                "Carol Diaz",
                "Thursday works",
                "One is fine, see you then.",
            ),
        ]
    } else {
        vec![arrival(
            "m1",
            101,
            "Anna Lee",
            "Lunch?",
            "Are you free Thursday around one? I can come to you.",
        )]
    };

    let banner: Banner = rule::digest(&arrivals, "alex@example.com", false).expect("a banner");
    let newest = arrivals.last().expect("an arrival");
    let target = PendingOpen {
        account_id: 1,
        thread_id: newest.thread_id,
        gmail_thread_id: newest.gmail_thread_id.clone(),
        at_ms: 0,
    };

    println!("title    {}", banner.title);
    println!("subtitle {:?}", banner.subtitle);
    println!("body     {:?}", banner.body);
    println!("target   thread_id={}", target.thread_id);

    let printer = Arc::new(Printer {
        opened: Mutex::new(Vec::new()),
    });
    notify::host::install(printer.clone());

    // Mach's own identifier, the same one `notify::host` passes. It only
    // resolves once `scripts/dev-bundle` has registered a `Mach.app`; without
    // that, `mac-notification-sys` falls back to Terminal's and `notify::mac`
    // prints why. See its module doc.
    let delivery = notify::mac::deliver(&banner, &target, IDENTIFIER);
    println!("delivery {delivery:?} — click the banner, or clear it, to finish");

    // `NSUserNotificationCenter` calls its delegate on the main thread, so the
    // click only comes back if the main run loop is turning. Tauri's main thread
    // is already doing that; this binary has no `NSApplication`, so without
    // this it sits there reporting no interaction however hard you click. The
    // printer above ends the process from the notification thread.
    unsafe { CFRunLoopRun() };
}

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRunLoopRun();
}

/// Says so, rather than exiting 0 as if a banner had been shown and clicked.
#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("notify_live: macOS only — this probe watches an NSUserNotification");
    std::process::exit(1);
}
