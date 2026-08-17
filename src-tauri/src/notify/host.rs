//! The platform, behind one trait.
//!
//! Everything above this file decides *what* to say. This is the only place
//! that knows there is a macOS, a Dock, a window that ⌘W may have hidden, and a
//! notification plugin — which is what lets `tests/notify.rs` drive the whole
//! decision path with a `Host` of its own and no application at all.
//!
//! # Why the plugin is registered here rather than in the builder chain
//!
//! `lib.rs` gets exactly one line: `.plugin(notify::init())`. That line buys
//! three things a bare `.plugin(tauri_plugin_notification::init())` would not:
//! the `AppHandle`, which the sync loop has no other route to; the
//! `threads-changed` subscription that keeps the Dock badge honest; and
//! `RunEvent::Reopen`, which is the only signal macOS gives us that a
//! notification was clicked. Wiring those anywhere else would mean editing the
//! `setup` hook and the run closure, which belong to another unit.
//!
//! The notification plugin itself is registered lazily, the first time a banner
//! is actually shown, and **not** from inside this plugin's `setup`:
//! `AppHandle::plugin` takes the plugin-store lock, and the store is already
//! holding it while it initialises us. The first banner runs on a sync worker,
//! where taking that lock is ordinary. The side effect is a pleasant one — an
//! app that never notifies never pays for the plugin.
//!
//! # Who actually draws the banner
//!
//! On macOS, not the plugin. [`super::mac`] calls `mac-notification-sys` — the
//! crate the plugin reaches through `notify-rust` anyway — because the plugin's
//! builder exposes neither the subtitle nor the interaction, and both are the
//! point. The plugin stays registered for the permission API alone, and never
//! shows anything, so only one of the two ever calls `set_application` and there
//! is nothing to conflict over.
//!
//! # What "permission refused" looks like on macOS today
//!
//! Not like an error. `tauri-plugin-notification`'s desktop path reports
//! `Granted` unconditionally, so a user who has switched Mach off in System
//! Settings produces *silence* rather than a refusal. What is new is that the
//! delivery itself now reports: `mac-notification-sys` returns a `Result`, and
//! [`super::mac`] says so on stderr instead of dropping it, which is the
//! difference between "no mail" and "could not say so" being distinguishable at
//! all. The permission dance below is written anyway, and honestly, because it
//! is what the API means and because the desktop implementation will not always
//! be this one. It costs one cached atomic read per banner.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, RwLock};

use serde::Serialize;
use tauri::plugin::{Builder, TauriPlugin};
use tauri::{AppHandle, Emitter, Listener, Manager, RunEvent, Runtime};

use crate::db::Db;

use super::{badge, rule::Banner, PendingOpen};

/// Carries the conversation a notification click should open. Named like
/// [`crate::shell::MENU_EVENT`], because it is the same kind of thing: the
/// shell telling the frontend that something outside the window happened.
pub const OPEN_EVENT: &str = "mach://notification-open";

/// Whether the OS will let this app interrupt the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    Granted,
    Denied,
    /// Never asked. Mach asks at the moment a notification would be shown, or
    /// when the setting is switched on — never at launch, where the prompt
    /// arrives before there is anything to explain it.
    Prompt,
}

/// What happened to a banner, and therefore what a click can still go through.
///
/// The distinction is the whole reason [`super::PendingOpen`] is now a fallback
/// rather than the mechanism: a watched banner routes its own click exactly, and
/// arming the guess as well would open the same conversation twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Shown, with a thread blocked on it. A click comes back to us and opens
    /// the conversation *that* banner was about. See [`super::mac`].
    Watched,
    /// Shown, but fire-and-forget. Only [`super::PendingOpen`] can carry a
    /// click, and only when the click activates the application.
    Unwatched,
    /// Nothing reached the screen: a QA instance, a refused permission, or a
    /// platform with no notifier.
    Silent,
}

/// Everything the notifier needs from the world outside it.
///
/// Object-safe and free of `Runtime`, so the sync loop can hold one without
/// becoming generic and a test can implement one in a dozen lines.
pub trait Host: Send + Sync + 'static {
    /// `target` is what a click on this banner should open. A host that can
    /// report clicks keeps it and answers [`Delivery::Watched`]; one that
    /// cannot ignores it and answers [`Delivery::Unwatched`], leaving the
    /// caller to arm the fallback.
    fn show(&self, banner: &Banner, target: &PendingOpen) -> Delivery;
    /// `None` clears the badge.
    fn set_badge(&self, count: Option<i64>);
    /// Show and focus the window, which ⌘W may have hidden.
    fn reopen(&self);
    fn open_conversation(&self, target: &PendingOpen);
    /// The store, once it exists. `None` before boot has finished.
    fn db(&self) -> Option<Db>;
    fn permission(&self) -> Permission;
    fn request_permission(&self) -> Permission;
}

// ===========================================================================
// The installed host
// ===========================================================================

static HOST: RwLock<Option<Arc<dyn Host>>> = RwLock::new(None);

/// Put a host in place. Called once by [`init`]; called by tests with their own.
pub fn install(host: Arc<dyn Host>) {
    *HOST.write().unwrap_or_else(|p| p.into_inner()) = Some(host);
}

/// The host, if there is one. `None` in unit tests, in `cargo test`, and in
/// the seconds before the window exists — all of which mean "say nothing",
/// which is the correct answer rather than a failure.
pub fn current() -> Option<Arc<dyn Host>> {
    HOST.read().unwrap_or_else(|p| p.into_inner()).clone()
}

/// Whether this process is allowed to put anything on the owner's screen.
///
/// A QA instance is not. `scripts/qa` starts a second Mach against its own
/// store so an agent can look at the real app; that instance is already
/// invisible — Accessory policy, unfocused window — and a banner would be the
/// one thing that reached across and interrupted the person using the machine.
/// See [`crate::shell::is_qa_instance`].
pub fn banners_allowed() -> bool {
    !crate::shell::is_qa_instance()
}

// ===========================================================================
// The Tauri host
// ===========================================================================

const UNKNOWN: u8 = 0;
const GRANTED: u8 = 1;
const DENIED: u8 = 2;
const PROMPT: u8 = 3;

struct TauriHost<R: Runtime> {
    app: AppHandle<R>,
    /// The permission as last observed, so a refused permission is asked about
    /// once per launch rather than on every pass. Reset by
    /// [`Host::request_permission`], which is what the settings dialog calls
    /// when the switch is turned back on.
    permission: AtomicU8,
}

impl<R: Runtime> TauriHost<R> {
    fn new(app: AppHandle<R>) -> Self {
        Self {
            app,
            permission: AtomicU8::new(UNKNOWN),
        }
    }

    /// Register `tauri-plugin-notification` if it is not registered yet. See
    /// the module doc for why this cannot happen during `setup`.
    fn ensure_plugin(&self) -> bool {
        if self
            .app
            .try_state::<tauri_plugin_notification::Notification<R>>()
            .is_some()
        {
            return true;
        }
        self.app
            .plugin(tauri_plugin_notification::init::<R>())
            .is_ok()
    }

    fn cached(&self) -> Option<Permission> {
        match self.permission.load(Ordering::Relaxed) {
            GRANTED => Some(Permission::Granted),
            DENIED => Some(Permission::Denied),
            PROMPT => Some(Permission::Prompt),
            _ => None,
        }
    }

    fn remember(&self, state: Permission) -> Permission {
        self.permission.store(
            match state {
                Permission::Granted => GRANTED,
                Permission::Denied => DENIED,
                Permission::Prompt => PROMPT,
            },
            Ordering::Relaxed,
        );
        state
    }
}

fn from_plugin(state: tauri_plugin_notification::PermissionState) -> Permission {
    use tauri_plugin_notification::PermissionState as P;
    match state {
        P::Granted => Permission::Granted,
        P::Denied => Permission::Denied,
        _ => Permission::Prompt,
    }
}

impl<R: Runtime> Host for TauriHost<R> {
    fn show(&self, banner: &Banner, target: &PendingOpen) -> Delivery {
        if !banners_allowed() {
            return Delivery::Silent;
        }
        // Asked here, at the moment a banner would appear, which is the only
        // moment the request explains itself.
        if !matches!(self.permission(), Permission::Granted) {
            return Delivery::Silent;
        }

        // The identifier `mac-notification-sys` will deliver under: Mach's own,
        // in a development build as much as a bundled one.
        //
        // This used to branch on `tauri::is_dev()` and hand macOS
        // `com.apple.Terminal` — copied from `tauri-plugin-notification`'s
        // desktop path, where it is there because a bare binary has no bundle
        // and `NSUserNotification` will not deliver for a process without a
        // bundle identifier. The cost was that every banner in the build the
        // owner actually runs arrived wearing Terminal's name and icon.
        //
        // What the identifier has to be is one **LaunchServices can resolve**,
        // not one the process owns: it is a swizzled string, which is exactly
        // why banners could wear Terminal's face with no Terminal involved.
        // `scripts/dev-bundle` registers a minimal `Mach.app` so this one
        // resolves. When it has not been run, `mac-notification-sys` falls back
        // to Terminal on its own and `notify::mac` says so.
        #[cfg(target_os = "macos")]
        {
            super::mac::deliver(banner, target, &self.app.config().identifier)
        }

        // Everywhere else the plugin is still the notifier. It has no subtitle,
        // so the three lines collapse into two, and it discards the interaction
        // — which leaves the fallback as all a click has.
        #[cfg(not(target_os = "macos"))]
        {
            let _ = target;
            use tauri_plugin_notification::NotificationExt;
            let body = match banner.subtitle.as_deref() {
                Some(subtitle) => format!("{subtitle}\n{}", banner.body),
                None => banner.body.clone(),
            };
            match self
                .app
                .notification()
                .builder()
                .title(banner.title.clone())
                .body(body)
                .show()
            {
                Ok(()) => Delivery::Unwatched,
                Err(error) => {
                    eprintln!("could not deliver a notification: {error}");
                    Delivery::Silent
                }
            }
        }
    }

    fn set_badge(&self, count: Option<i64>) {
        if !banners_allowed() {
            return;
        }
        // Any window will do — on macOS the badge belongs to the application's
        // Dock tile, not to a window — but Mach only ever has the one.
        if let Some(window) = self.app.webview_windows().values().next() {
            let _ = window.set_badge_count(count);
        }
    }

    fn reopen(&self) {
        // Hopped to the main thread on purpose. This used to be reachable only
        // from `RunEvent::Reopen`, which is already there; a click routed by
        // `notify::mac` arrives on the notification thread instead, and window
        // calls are the last thing to find out about off the main thread the
        // hard way.
        let app = self.app.clone();
        let _ = self
            .app
            .run_on_main_thread(move || crate::shell::reopen(&app));
    }

    fn open_conversation(&self, target: &PendingOpen) {
        let _ = self.app.emit(OPEN_EVENT, target);
    }

    fn db(&self) -> Option<Db> {
        self.app
            .try_state::<crate::ipc::AppState>()
            .map(|state| state.db.clone())
    }

    fn permission(&self) -> Permission {
        if let Some(known) = self.cached() {
            return known;
        }
        if !self.ensure_plugin() {
            return self.remember(Permission::Denied);
        }
        use tauri_plugin_notification::NotificationExt;
        let state = self
            .app
            .notification()
            .permission_state()
            .map(from_plugin)
            .unwrap_or(Permission::Denied);

        match state {
            Permission::Prompt => self.request_permission(),
            other => self.remember(other),
        }
    }

    fn request_permission(&self) -> Permission {
        self.permission.store(UNKNOWN, Ordering::Relaxed);
        if !self.ensure_plugin() {
            return self.remember(Permission::Denied);
        }
        use tauri_plugin_notification::NotificationExt;
        let state = self
            .app
            .notification()
            .request_permission()
            .map(from_plugin)
            .unwrap_or(Permission::Denied);
        self.remember(state)
    }
}

// ===========================================================================
// The plugin
// ===========================================================================

/// The one line `lib.rs` adds to the builder chain.
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("mach-notify")
        .setup(|app, _api| {
            let host = Arc::new(TauriHost::new(app.clone()));
            install(host.clone());

            // The badge follows the store, and `threads-changed` is already
            // emitted by every path that writes to it — a sync pass, a command,
            // the agent. Subscribing is how this module stays out of all three.
            app.listen(crate::ipc::events::THREADS_CHANGED_EVENT, move |_| {
                let host = host.clone();
                // Off the event thread: the recompute is a database read, and
                // the loop that delivers events also draws the window.
                tauri::async_runtime::spawn(async move { badge::refresh(host.as_ref()) });
            });
            Ok(())
        })
        .on_event(|_app, event| match event {
            // The first honest badge: the store is open and the accounts are
            // restored, and nothing has necessarily written yet.
            RunEvent::Ready => {
                if let Some(host) = current() {
                    badge::refresh(host.as_ref());
                }
            }
            // A Dock click, `open -a Mach`, or a click on one of our banners —
            // macOS does not distinguish, and the notification carries nothing
            // back that would let us. Focus the window either way; open the
            // conversation only if we said something about one recently enough
            // for this to plausibly be the answer to it.
            RunEvent::Reopen { .. } => {
                if let Some(host) = current() {
                    host.reopen();
                    if let Some(target) = super::take_pending_open() {
                        host.open_conversation(&target);
                    }
                }
            }
            _ => {}
        })
        .build()
}
