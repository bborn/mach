//! Plugins — the manifest, the origin they run on, and where they live.
//!
//! The design is `docs/plugins.md`; this module is the half of it that has to be
//! in Rust, which is exactly three things and no more:
//!
//! | module | what lives there |
//! |---|---|
//! | [`manifest`] | the schema, the `machApi` gate, the proposed-API gate, and the install prompt's sentences |
//! | [`protocol`] | the `plugin://<id>/` custom scheme, and the CSP that makes it a sandbox |
//! | [`store`] | the plugin directory, and the content-addressed approval record |
//! | [`runtime`] | what is installed, whether the sandbox was verified, and the bridge the agent calls through |
//!
//! Everything else — the guest, the worker, the capability check on each
//! `mach.*` call, the views — is in `src/lib/plugins/`, because that is where
//! the iframe is. The split is not arbitrary: **the filesystem, the approval
//! record and the agent's tool list are Rust's**, because they outlive a window
//! and because the command layer is the trust boundary that actually matters;
//! **the sandbox is the frontend's**, because it is a DOM object.
//!
//! # What a plugin never gets
//!
//! No Google client, no OAuth token, no `invoke`, no SQL, no filesystem, no
//! network. It composes [`crate::commands::Command`] through the same dispatcher
//! the keyboard uses, and inherits undo, rollback, audit and rate limiting for
//! free. That is the whole security model, and it is the part that was already
//! built.

pub mod manifest;
pub mod protocol;
pub mod runtime;
pub mod store;

pub use manifest::{
    consent_lines, ConsentLine, InstallKind, ManifestError, PluginManifest, Severity,
};
pub use protocol::{GUEST_CSP, GUEST_SANDBOX, SCHEME};
pub use runtime::{ConformanceReport, InvokeRequest, InvokeSink, PluginRuntime};
pub use store::{InstalledPlugin, PluginStatus, PluginStore, StoreError};
