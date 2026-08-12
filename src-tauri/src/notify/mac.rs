//! Delivering a banner on macOS, and hearing about the click.
//!
//! This module exists because of one measured fact: `tauri-plugin-notification`
//! throws the interaction away, but the crate underneath it does not.
//!
//! ```text
//!   tauri-plugin-notification ──► notify-rust ──► mac-notification-sys ──► NSUserNotification
//!        (title, body only,            (adds          (adds subtitle,
//!         result discarded)             subtitle)      buttons, and the
//!                                                      interaction itself)
//! ```
//!
//! `mac_notification_sys::Notification::send` returns a
//! [`NotificationResponse`], and with `wait_for_click(true)` it blocks until the
//! user clicks the banner, dismisses it, or clears it out of Notification
//! Centre. That response is what makes an *exact* click possible: it carries no
//! identifier, but it arrives on the thread that sent that one banner, and that
//! thread closed over the conversation the banner was about.
//!
//! # What was measured, in the build the owner actually runs
//!
//! Mach in development is the bare `target/debug/mach`, not a `.app`. That
//! matters, because `NSUserNotification` refuses to deliver for a process with
//! no bundle identifier. `mac-notification-sys` works around it by swizzling
//! `-[NSBundle bundleIdentifier]` to return a *borrowed* one, and
//! `tauri-plugin-notification` picks `com.apple.Terminal` when `tauri::is_dev()`.
//! Keeping that exact choice is why banners still arrive at all here. Measured
//! on macOS 15.2, against this dependency tree:
//!
//! * the banner **is** delivered — `usernoted` logs it as
//!   `Presenting <NotificationRecord app:"com.apple.Terminal" …> as banner`;
//! * `subtitle` renders, as a second bold line above the body;
//! * a click on it comes back as `Ok(NotificationResponse::Click)`;
//! * and it does **not** launch or activate Terminal. Because this process holds
//!   the notification-centre connection for that identifier, macOS routes the
//!   activation here rather than to the app whose name is on it. So the window
//!   that comes forward is ours, raised by [`Host::reopen`] below, and never
//!   somebody else's terminal.
//!
//! The borrowed identifier is still visible: the banner wears Terminal's icon in
//! a development build. A bundled build uses the real identifier and its own
//! icon, and nothing else about this path changes.
//!
//! # Why one thread per banner, and why they are capped
//!
//! `send` blocks, so it can be on neither the sync thread that called it nor the
//! main thread that draws the window — hence a thread of its own. It stays
//! blocked for as long as the notification sits in Notification Centre, which
//! for an ignored banner means until the owner clears it. There is no cancel in
//! this API, so the count is capped: past [`MAX_WATCHED`], banners go out
//! fire-and-forget and fall back to [`super::PendingOpen`].
//!
//! # The one thing this needs from its host process
//!
//! `NSUserNotificationCenter` delivers its delegate callbacks on the **main**
//! thread, so a `send` running anywhere else only ever hears about a click if
//! the main run loop is turning. In the app that is free — the main thread is
//! `NSApplication`'s — and it is the reason this cannot simply be moved into a
//! plain worker binary. `src/bin/notify_live.rs` has to spin one by hand, and
//! silently answers "no interaction" to every click until it does, which is
//! exactly how this was found.
//!
//! # The one thing this cannot tell apart
//!
//! The delegate that receives the click is a property of the *shared*
//! `NSUserNotificationCenter`, and each send installs a fresh one, so the newest
//! send is the only one macOS will report a click to — whichever banner was
//! actually clicked. When exactly one banner is outstanding that is unambiguous.
//! When several are, a click could have been on any of them, and this module
//! raises the window without selecting a conversation rather than open one the
//! owner did not ask for.

use std::sync::atomic::{AtomicUsize, Ordering};

use mac_notification_sys::{set_application, Notification, NotificationResponse};

use super::host::{self, Delivery};
use super::{rule::Banner, PendingOpen};

/// How many banners may have a thread blocked on them at once.
///
/// Each one costs a parked thread until the owner clears it out of Notification
/// Centre, which can be days. Four covers the case this is for — a banner
/// arrives, and is clicked or ignored within the minute — and puts a ceiling on
/// the case it is not.
const MAX_WATCHED: usize = 4;

static WATCHED: AtomicUsize = AtomicUsize::new(0);

/// Show a banner, and arrange for its click to open `target`.
///
/// `identifier` is the bundle identifier to deliver under — see the module doc
/// on why a development build borrows one. Returns without waiting: the
/// blocking half is on a thread of its own.
pub fn deliver(banner: &Banner, target: &PendingOpen, identifier: &str) -> Delivery {
    // Installed once per process, by whichever banner is first. `Err` here is
    // the ordinary "already set" on every banner after that; a genuine failure
    // shows up as a delivery error below, which is the one worth saying out
    // loud.
    let _ = set_application(identifier);

    let watched = claim_watch_slot();
    let (title, subtitle, body) = (
        banner.title.clone(),
        banner.subtitle.clone(),
        banner.body.clone(),
    );
    let target = target.clone();

    let spawned = std::thread::Builder::new()
        .name("mach-notify".into())
        .spawn(move || {
            let mut notification = Notification::new();
            notification.title(&title).message(&body);
            if let Some(subtitle) = subtitle.as_deref() {
                notification.subtitle(subtitle);
            }
            if watched {
                notification.wait_for_click(true);
            } else {
                notification.asynchronous(true);
            }

            let response = notification.send();
            if watched {
                // Before routing, so a panic in the frontend hop cannot leak the
                // slot and quietly turn every later banner into a fallback.
                WATCHED.fetch_sub(1, Ordering::SeqCst);
            }

            match response {
                Ok(NotificationResponse::Click) if watched => route_click(&target),
                Ok(_) => {}
                // Failure must be visible. The plugin discarded this, which is
                // why a Mach that could not deliver a single banner looked
                // exactly like a Mach with no new mail.
                Err(error) => {
                    eprintln!("could not deliver a notification: {error}");
                }
            }
        });

    match spawned {
        Ok(_) => {
            if watched {
                Delivery::Watched
            } else {
                Delivery::Unwatched
            }
        }
        Err(error) => {
            if watched {
                WATCHED.fetch_sub(1, Ordering::SeqCst);
            }
            eprintln!("could not start the notification thread: {error}");
            Delivery::Silent
        }
    }
}

/// Take one of the [`MAX_WATCHED`] slots, if there is one going.
fn claim_watch_slot() -> bool {
    WATCHED
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |watched| {
            (watched < MAX_WATCHED).then_some(watched + 1)
        })
        .is_ok()
}

/// Route a click that macOS has already reported against `target`'s banner.
///
/// The window comes forward either way — the owner clicked a banner, so they
/// want the app. Whether a *conversation* opens depends on whether this click
/// can only have been about one banner; see the module doc.
///
/// Public so the routing can be tested without a macOS in the loop: the half
/// worth asserting on is "a click naming this conversation opens this
/// conversation", and that half is ordinary Rust.
pub fn route_click(target: &PendingOpen) {
    let Some(host) = host::current() else {
        return;
    };
    host.reopen();

    // Read after the slot was released, so zero means "this was the only banner
    // outstanding" and the click cannot have been about another one.
    if WATCHED.load(Ordering::SeqCst) == 0 {
        host.open_conversation(target);
    }
}
