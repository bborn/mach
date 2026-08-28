//! What a tool's answer looks like to a person.
//!
//! # Generic on purpose
//!
//! There is no `fn render_threads`, no `fn render_events`. Every tool already
//! produces a one-line `summary` and a payload that is an object with one array
//! in it, or one object's worth of fields — so this prints the summary and then
//! draws whatever the payload actually contains. A tool added to the surface
//! renders on the day it is added, and a payload that grows a field shows the
//! field.
//!
//! The one convention it leans on is the codebase's own: a timestamp is unix
//! milliseconds and its key ends in `Ms`, or is `at`. That is documented in
//! `ipc`'s module header as the wire contract, so reading it here is following
//! a rule rather than guessing at one — and it is the difference between a
//! column of 1756312800000 and a column of dates.

use chrono::{Local, TimeZone};
use serde_json::Value;

/// The longest a single cell is allowed to be before it is cut. A subject line
/// runs to hundreds of characters and one of them must not push every other
/// column off the screen.
const CELL: usize = 48;

/// The width assumed when stdout is not a terminal.
///
/// A pipe has no width, and reflowing for one would be guessing at a reader who
/// is very often `grep`. So a redirected `mach` always gets the table, whatever
/// its width — the columns are the machine-readable half of the human output,
/// and `awk` wants them present more than it wants them narrow.
const PIPED: usize = usize::MAX;

/// A tool outcome, for a terminal.
pub fn outcome(summary: &str, payload: &Value) -> String {
    let mut out = String::new();
    if !summary.is_empty() {
        out.push_str(summary);
        out.push('\n');
    }
    let body = body(payload);
    if !body.is_empty() {
        out.push('\n');
        out.push_str(&body);
    }
    out
}

fn body(payload: &Value) -> String {
    let Some(object) = payload.as_object() else {
        return pretty(payload);
    };

    // The overwhelmingly common shape: `{ "threads": [...] }`. One array and
    // nothing else means the answer *is* the array.
    let arrays: Vec<(&String, &Vec<Value>)> = object
        .iter()
        .filter_map(|(k, v)| v.as_array().map(|a| (k, a)))
        .collect();
    if let [(_, rows)] = arrays.as_slice() {
        if object.len() == 1 {
            return rows_for(rows, terminal_width());
        }
    }

    // Otherwise it is one object's fields — `get_thread`, `get_event` — with
    // whatever nesting they have.
    fields(object)
}

/// One row per key, aligned. Nested values are printed as compact JSON: an
/// array of messages is not a thing a terminal can usefully align, and the
/// caller who wants it has `--json`.
fn fields(object: &serde_json::Map<String, Value>) -> String {
    let width = object.keys().map(String::len).max().unwrap_or(0);
    object
        .iter()
        .map(|(key, value)| format!("{key:<width$}  {}", cell(key, value)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A table when it fits the terminal, and one block per row when it does not.
///
/// Twenty conversations with a subject, a snippet, a sender and a label set is
/// two hundred and thirty columns of table, and a terminal wraps that into
/// something nobody can read a row out of. The fallback is not a different set
/// of facts — it is the same cells, one per line, with a blank line between
/// records. Which one you get depends only on the window, so a screenshot of
/// either is a screenshot of the same answer.
fn rows_for(rows: &[Value], width: usize) -> String {
    let table = table(rows);
    let widest = table.lines().map(|l| l.chars().count()).max().unwrap_or(0);
    match widest > width {
        true => records(rows),
        false => table,
    }
}

/// One block per row: every field on its own line, keys aligned.
fn records(rows: &[Value]) -> String {
    rows.iter()
        .map(|row| match row.as_object() {
            Some(object) => fields(object),
            None => pretty(row),
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// How wide the terminal is, or [`PIPED`] when there is not one.
///
/// `TIOCGWINSZ` on stdout rather than the `COLUMNS` variable, because a shell
/// does not export `COLUMNS` to the processes it starts — reading it would mean
/// reflowing for a width that is almost always absent and occasionally stale.
#[cfg(unix)]
fn terminal_width() -> usize {
    // Safety: `ioctl(TIOCGWINSZ)` writes a `winsize` and nothing else. The
    // struct is zeroed first, so a driver that fills in less than all of it
    // leaves zeros rather than whatever was on the stack.
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) } == 0;
    match ok && size.ws_col > 0 {
        true => size.ws_col as usize,
        false => PIPED,
    }
}

#[cfg(not(unix))]
fn terminal_width() -> usize {
    PIPED
}

fn table(rows: &[Value]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    // Column order from the first row, then anything a later row adds. Rows out
    // of one tool are the same shape, so this is nearly always just the first
    // row's keys — but a row with an extra field must not lose it silently.
    let mut columns: Vec<String> = Vec::new();
    for row in rows {
        if let Some(object) = row.as_object() {
            for key in object.keys() {
                if !columns.contains(key) {
                    columns.push(key.clone());
                }
            }
        }
    }
    if columns.is_empty() {
        return rows.iter().map(pretty).collect::<Vec<_>>().join("\n");
    }

    let mut grid: Vec<Vec<String>> = vec![columns.clone()];
    for row in rows {
        grid.push(
            columns
                .iter()
                .map(|key| {
                    row.get(key)
                        .map(|value| cell(key, value))
                        .unwrap_or_default()
                })
                .collect(),
        );
    }

    let widths: Vec<usize> = (0..columns.len())
        .map(|i| grid.iter().map(|row| row[i].chars().count()).max().unwrap_or(0))
        .collect();

    grid.iter()
        .map(|row| {
            row.iter()
                .enumerate()
                .map(|(i, value)| {
                    let pad = widths[i].saturating_sub(value.chars().count());
                    match i + 1 == columns.len() {
                        // No trailing whitespace on the last column.
                        true => value.clone(),
                        false => format!("{value}{}", " ".repeat(pad)),
                    }
                })
                .collect::<Vec<_>>()
                .join("  ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One value, as a string short enough to sit in a column.
fn cell(key: &str, value: &Value) -> String {
    let text = match value {
        Value::Null => String::new(),
        Value::Bool(b) => match b {
            true => "yes".to_string(),
            false => String::new(),
        },
        Value::Number(n) => match n.as_i64().filter(|_| is_time(key)) {
            Some(ms) => time(ms),
            None => n.to_string(),
        },
        Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    };
    let flat = text.replace(['\n', '\r', '\t'], " ");
    match flat.chars().count() > CELL {
        true => format!("{}…", flat.chars().take(CELL - 1).collect::<String>()),
        false => flat,
    }
}

/// Whether a key names a unix-millisecond timestamp, by the wire convention in
/// `ipc`'s module header.
fn is_time(key: &str) -> bool {
    key == "at" || key.ends_with("Ms") || key.ends_with("At")
}

/// A timestamp in the reader's own zone. Bounded so that a number that happens
/// to end up under a `Ms` key but is not a time — a duration, a count — is
/// printed as itself rather than as a date in 1970.
fn time(ms: i64) -> String {
    const PLAUSIBLE: std::ops::Range<i64> = 100_000_000_000..4_000_000_000_000;
    if !PLAUSIBLE.contains(&ms) {
        return ms.to_string();
    }
    match Local.timestamp_millis_opt(ms).single() {
        Some(when) => when.format("%Y-%m-%d %H:%M").to_string(),
        None => ms.to_string(),
    }
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn one_array_becomes_a_table_with_a_header() {
        let payload = json!({
            "accounts": [
                { "accountId": 1, "email": "alex@lumen.example" },
                { "accountId": 2, "email": "molly@example.com" },
            ]
        });
        let out = outcome("2 accounts", &payload);
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines[0], "2 accounts");
        assert!(lines[2].starts_with("accountId"), "{out}");
        assert!(lines[3].contains("alex@lumen.example"), "{out}");
    }

    #[test]
    fn a_millisecond_key_is_printed_as_a_time_and_a_small_number_is_not() {
        assert!(cell("startMs", &json!(1_756_312_800_000i64)).contains('-'));
        assert_eq!(cell("startMs", &json!(30)), "30");
        assert_eq!(cell("limit", &json!(1_756_312_800_000i64)), "1756312800000");
    }

    #[test]
    fn a_long_subject_is_cut_rather_than_wrapped() {
        let long = "x".repeat(400);
        let out = cell("subject", &json!(long));
        assert!(out.chars().count() <= CELL, "{}", out.chars().count());
        assert!(out.ends_with('…'));
    }

    #[test]
    fn a_table_too_wide_for_the_window_becomes_one_block_per_row() {
        let rows = vec![
            json!({ "subject": "Series A data room", "from": "Tawny Chen", "threadId": 41 }),
            json!({ "subject": "Dinner", "from": "Molly", "threadId": 42 }),
        ];
        // Wide enough: a header line and two rows.
        let wide = rows_for(&rows, 200);
        assert_eq!(wide.lines().count(), 3, "{wide}");
        assert!(wide.starts_with("from"), "{wide}");

        // Narrow: three fields each, with a blank line between the two records.
        let narrow = rows_for(&rows, 20);
        assert_eq!(narrow.lines().count(), 7, "{narrow}");
        assert!(narrow.contains("Series A data room"), "{narrow}");
        assert!(narrow.contains("Dinner"), "{narrow}");
    }

    #[test]
    fn one_object_is_printed_as_its_fields() {
        let payload = json!({ "threadId": 4127, "subject": "Data room" });
        let out = outcome("Read \u{201c}Data room\u{201d}", &payload);
        assert!(out.contains("threadId"), "{out}");
        assert!(out.contains("4127"), "{out}");
    }
}
