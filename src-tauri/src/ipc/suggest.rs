//! Reply suggestions: the invoke surface.
//!
//! | command | argument | returns |
//! |---|---|---|
//! | `reply_suggestions` | `threadId` | `{ stances, messageId, model }` or `null` |
//! | `reply_suggestion_record` | `kind`, `stanceIndex?`, `stanceLabel?` | `{ ok }` |
//! | `reply_suggestion_stats` | — | counters, the rate, and the winning labels |
//!
//! Three reads and one tiny write, all local. There is deliberately no command
//! that *generates*: generation is the sync loop's, off the arrival of a
//! message, and a webview that could ask for one would be a way to spend money
//! by opening a conversation.
//!
//! `reply_suggestions` is the read behind the stance row, and it is the reason
//! picking one is instant — the bodies are already on disk when the row draws,
//! so the keystroke is a state change and not a request.

use serde_json::{json, Value};
use tauri::State;

use crate::suggest::{store, Outcome};

use super::error::IpcError;
use super::state::AppState;

/// The stances for a conversation, if they still answer its newest message.
///
/// `null` covers every reason there are none — never generated, generated and
/// gone stale, feature switched off — because the row has one behaviour for all
/// of them: it is not there.
#[tauri::command]
pub async fn reply_suggestions(
    state: State<'_, AppState>,
    thread_id: i64,
) -> Result<Value, IpcError> {
    let found = state
        .db
        .read(|conn| store::fresh_for_thread(conn, thread_id))?;
    Ok(match found {
        Some(suggestion) => json!({
            "threadId": suggestion.thread_id,
            "messageId": suggestion.message_id,
            "model": suggestion.model,
            "createdAt": suggestion.created_at,
            "stances": suggestion.stances,
        }),
        None => Value::Null,
    })
}

/// Note what he did with a set of stances.
///
/// Never an error the caller has to handle: a counter that fails is a counter,
/// and refusing the keystroke because the statistics could not be updated would
/// be the tail wagging the dog. An unknown `kind` is dropped rather than stored,
/// so a typo cannot invent a sixth column in the preferences panel.
#[tauri::command]
pub async fn reply_suggestion_record(
    state: State<'_, AppState>,
    kind: String,
    stance_index: Option<i64>,
    stance_label: Option<String>,
    thread_id: Option<i64>,
) -> Result<Value, IpcError> {
    let Some(outcome) = Outcome::parse(&kind) else {
        return Ok(json!({ "ok": false }));
    };
    let label = stance_label.unwrap_or_default();
    let now = crate::suggest::now_ms();

    state.db.write(move |conn| {
        store::record(conn, outcome, stance_index, &label, now)?;
        // Once a stance has been sent, the conversation has been answered and
        // there is nothing left to suggest about it. Dropping the row here
        // rather than waiting for the sent message to land keeps the composer
        // from closing onto a strip offering the same three buttons again.
        if matches!(outcome, Outcome::SentAsWritten | Outcome::SentEdited) {
            if let Some(thread_id) = thread_id {
                store::forget(conn, thread_id)?;
            }
        }
        Ok(())
    })?;
    Ok(json!({ "ok": true }))
}

/// How the feature is doing. Local, and the only place the answer exists.
#[tauri::command]
pub async fn reply_suggestion_stats(state: State<'_, AppState>) -> Result<Value, IpcError> {
    let (counters, labels) = state.db.read(|conn| {
        let counters = store::counters(conn)?;
        let labels = store::winning_labels(conn, 5)?;
        Ok((counters, labels))
    })?;

    Ok(json!({
        "suggested": counters.suggested,
        "picked": counters.picked,
        "sentAsWritten": counters.sent_as_written,
        "sentEdited": counters.sent_edited,
        "dismissed": counters.dismissed,
        "asWrittenRate": counters.as_written_rate(),
        "winningLabels": labels
            .into_iter()
            .map(|(label, count)| json!({ "label": label, "count": count }))
            .collect::<Vec<_>>(),
    }))
}
