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

use std::sync::{Arc, Mutex};

use mach_lib::db::Db;
use mach_lib::notify::{
    self,
    host::{Delivery, Host},
    rule::{self, Arrival},
    Banner, PendingOpen, Permission,
};

/// A host that only records, so the click can be printed rather than emitted
/// into a window this process does not have.
struct Printer {
    opened: Mutex<Vec<PendingOpen>>,
}

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

    // The identifier a development build borrows; see `notify::mac`.
    let delivery = notify::mac::deliver(&banner, &target, "com.apple.Terminal");
    println!("delivery {delivery:?} — click the banner, or clear it, to finish");

    // `NSUserNotificationCenter` calls its delegate on the main thread, so the
    // click only comes back if the main run loop is turning. Tauri's main thread
    // is already doing that; this binary has no `NSApplication`, so without
    // this it sits there reporting no interaction however hard you click. The
    // printer above ends the process from the notification thread.
    unsafe { CFRunLoopRun() };
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRunLoopRun();
}
