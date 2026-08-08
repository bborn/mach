//! Notifications, from the frontend's side of the wire.
//!
//! | command | argument | returns |
//! |---|---|---|
//! | `notification_state` | — | `{ permission, available }` |
//! | `notification_request_permission` | — | `{ permission, available }` |
//! | `notification_pending_open` | — | `PendingOpen \| null` |
//!
//! Three commands, and none of them decides anything. Whether a message earns a
//! banner is settled in [`crate::notify::rule`] on the Rust side, because the
//! only thing that knows a message *arrived* — rather than merely being in the
//! store — is the sync loop. What the frontend gets is the two questions it
//! genuinely cannot answer for itself:
//!
//!  * **may we?** — so ⌘, can say "macOS is not letting Mach do this" instead of
//!    showing a switch that is on and does nothing. The request is made from
//!    here, when the switch is turned on, which is a moment the prompt explains
//!    itself; never at launch.
//!  * **what should be open?** — the conversation a notification click was
//!    about. See [`crate::notify::PendingOpen`] for why a click cannot simply
//!    carry it.
//!
//! There is deliberately no "send a test notification" command. The only thing
//! it would be used for is firing a banner at somebody who did not get mail.

use serde::Serialize;

use crate::notify::{self, PendingOpen, Permission};

/// What ⌘, needs to describe the state of this feature in one line.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationState {
    pub permission: Permission,
    /// False in a QA instance, and in any build with no window yet — a switch
    /// that is on but cannot deliver should say so rather than lie quietly.
    pub available: bool,
}

fn state() -> NotificationState {
    let permission = notify::host::current()
        .map(|host| host.permission())
        .unwrap_or(Permission::Prompt);
    NotificationState {
        permission,
        available: notify::host::banners_allowed() && notify::host::current().is_some(),
    }
}

/// Whether macOS will deliver, without asking it anything it has not been asked
/// already. Safe to call on every render of the dialog.
#[tauri::command]
pub fn notification_state() -> NotificationState {
    state()
}

/// Ask for permission now, because the user just turned the switch on.
#[tauri::command]
pub fn notification_request_permission() -> NotificationState {
    if let Some(host) = notify::host::current() {
        host.request_permission();
    }
    state()
}

/// The conversation a notification click should open, consumed.
///
/// Returns `null` almost every time it is called, which is correct: it is the
/// answer to "did the user just come back from a banner", and usually they did
/// not. The push event [`crate::notify::host::OPEN_EVENT`] carries the same
/// value the moment it appears; this command exists so a window that was still
/// starting up when that event fired can still find it.
#[tauri::command]
pub fn notification_pending_open() -> Option<PendingOpen> {
    notify::take_pending_open()
}
