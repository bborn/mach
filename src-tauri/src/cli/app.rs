//! The program: one invocation, one answer, one exit code.
//!
//! # The two routes, and why a read never knocks
//!
//! A verb in [`tools::READ_TOOLS`] is answered here, in this process, out of
//! SQLite opened with [`Db::open_read_only`]. It does not ask whether Mach is
//! running and it does not care: WAL means a second reader never blocks the app
//! and is never blocked by it, so the cheapest correct thing is also the fastest
//! one, and `mach search invoice` works on a laptop where the app was never
//! opened today.
//!
//! Everything else goes over the door, and if the app is not running it **fails
//! and says so**. It does not queue and it does not half-succeed. Only the
//! running process holds the OAuth tokens, the outbox's recall timer and the
//! undo stack; a second writer would break the one-writer invariant `db`
//! makes a type-level fact, and — worse — a local write Google never accepted
//! has no revert path outside the app, while `users.history.list` only reports
//! changes that *happened*. Such a write would survive silently until a full
//! resync. "Failure must be visible" is the standing rule and this is the case
//! it was written for.
//!
//! # The verbs come from the app
//!
//! [`surface`] asks the door for [`ToolGate::tools`] when the app is up, and
//! falls back to [`tools::tools`] when it is not. Both are the same generated
//! list; the door's is wider only because it includes installed plugins, which
//! is a fact about a running process and cannot be known otherwise. Nothing in
//! this file names a verb.
//!
//! [`ToolGate::tools`]: crate::ipc::agent::engine::gate::ToolGate::tools

use std::path::PathBuf;

use serde_json::{json, Value};

use crate::db::Db;
use crate::ipc::agent::engine::tools;

use super::args::{self, Globals};
use super::client::Client;
use super::endpoint;
use super::error::CliError;
use super::protocol::{self, Class, Consent, DoorRequest};
use super::render;

/// Words that are the command line talking about itself rather than about mail.
///
/// Checked before the surface, so a tool could in principle shadow one. None
/// does, and `no_tool_is_named_after_a_reserved_word` keeps it that way — the
/// alternative, checking the surface first, would mean a plugin could take
/// `help` away from the operator.
const RESERVED: &[&str] = &["tools", "help", "where"];

/// The whole program. Returns the exit code; see [`CliError::exit_code`].
pub fn main() -> i32 {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let (globals, rest) = args::take_globals(&argv);

    match run(&globals, &rest) {
        Ok(text) => {
            if !text.is_empty() {
                println!("{}", text.trim_end());
            }
            0
        }
        Err(error) => {
            // In `--json` the error goes to stdout as JSON, so a caller parses
            // one stream and never has to correlate two. In human mode it goes
            // to stderr, where a shell expects it.
            match globals.json {
                true => println!("{}", json!({ "ok": false, "error": error })),
                false => eprintln!("{error}"),
            }
            error.exit_code()
        }
    }
}

fn run(globals: &Globals, rest: &[String]) -> Result<String, CliError> {
    let data_dir = endpoint::default_data_dir().map_err(|e| CliError::new("store", e))?;

    let Some(word) = rest.first().cloned() else {
        return usage(&data_dir, globals);
    };
    if globals.help && rest.len() == 1 {
        return help_for(&data_dir, globals, &word);
    }

    match word.as_str() {
        "help" => match rest.get(1) {
            Some(verb) => help_for(&data_dir, globals, verb),
            None => usage(&data_dir, globals),
        },
        "tools" => list_tools(&data_dir, globals),
        "where" => Ok(where_things_are(&data_dir, globals)),
        _ => call(&data_dir, globals, &word, &rest[1..]),
    }
}

// ===========================================================================
// the surface
// ===========================================================================

/// One verb, as the app describes it.
struct Verb {
    name: String,
    description: String,
    schema: Value,
    consent: Class,
    local: bool,
}

/// Everything this app can be asked to do, and where the list came from.
///
/// `Err` is never returned for "the app is not running": the core surface is a
/// pure function and answering `mach tools` with the app closed is worth more
/// than being precise about plugins. What is lost is named in the output.
fn surface(data_dir: &PathBuf) -> (Vec<Verb>, Option<Client>) {
    if let Ok(client) = Client::locate(data_dir.clone()) {
        if let Ok(answer) = client.ask(&DoorRequest::Tools) {
            if let Some(items) = answer.get("tools").and_then(Value::as_array) {
                let verbs = items
                    .iter()
                    .filter_map(|item| {
                        let name = item.get("name")?.as_str()?.to_string();
                        Some(Verb {
                            description: item
                                .get("description")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            schema: item.get("inputSchema").cloned().unwrap_or(Value::Null),
                            consent: protocol::classify(&name),
                            local: item
                                .get("local")
                                .and_then(Value::as_bool)
                                .unwrap_or_else(|| protocol::is_local(&name)),
                            name,
                        })
                    })
                    .collect();
                return (verbs, Some(client));
            }
        }
    }
    // The app is not up, or the door would not talk. The core surface is still
    // knowable — it is generated from the catalogue at compile time — and every
    // read in it still works.
    let verbs = tools::tools()
        .into_iter()
        .map(|tool| Verb {
            consent: protocol::classify(&tool.definition.name),
            local: protocol::is_local(&tool.definition.name),
            name: tool.definition.name,
            description: tool.definition.description,
            schema: tool.definition.input_schema,
        })
        .collect();
    (verbs, None)
}

// ===========================================================================
// calling one
// ===========================================================================

fn call(
    data_dir: &PathBuf,
    globals: &Globals,
    typed: &str,
    tokens: &[String],
) -> Result<String, CliError> {
    let (verbs, client) = surface(data_dir);
    let names: Vec<String> = verbs.iter().map(|v| v.name.clone()).collect();
    let name = args::resolve_verb(typed, &names)?;
    let verb = verbs
        .iter()
        .find(|v| v.name == name)
        .expect("resolve_verb returns a name from the list it was given");

    // `--to` is the recipient confirmation on an outbound verb and an ordinary
    // parameter everywhere else. See `args::take_recipients`.
    let (recipients, tokens) = match verb.consent {
        Class::Outbound => {
            let (recipients, rest) = args::take_recipients(tokens)?;
            (Some(recipients), rest)
        }
        _ => (None, tokens.to_vec()),
    };

    let input = args::build_input(&tokens, &verb.schema)?;

    if verb.local {
        return local_call(data_dir, &name, &input, globals);
    }

    // `surface` fell back to the built-in list, which means the door was not
    // reachable. Ask again rather than inventing a sentence: `Client::locate`
    // knows which of the three failures it was, and the operator needs the
    // specific one.
    let client = match client {
        Some(client) => client,
        None => Client::locate(data_dir.clone())?,
    };
    let consent = Consent {
        // `MACH_CLI_YES=1` authorises mutation and never a send; see
        // `protocol`. The door decides, not this.
        mutate: globals.yes || env_yes(),
        recipients,
    };
    let answer = client.ask(&DoorRequest::Call {
        tool: name.clone(),
        input,
        consent,
    })?;
    Ok(present(globals, &answer))
}

/// A read, answered here, from a store nothing in this process can write to.
fn local_call(
    data_dir: &PathBuf,
    name: &str,
    input: &Value,
    globals: &Globals,
) -> Result<String, CliError> {
    let path = crate::config::database_path(data_dir);
    let db = Db::open_read_only(&path).map_err(|e| {
        CliError::new(
            "store",
            format!("could not read {}: {e}", path.display()),
        )
    })?;

    let outcome = tools::execute_read(&db, name, input)
        .expect("a local verb is one execute_read answers")
        .map_err(|e| CliError::new(e.kind(), e.to_string()))?;

    Ok(present(
        globals,
        &json!({
            "ok": true,
            "tool": name,
            "summary": outcome.summary,
            "payload": outcome.payload,
            "mutated": outcome.mutated,
        }),
    ))
}

/// `MACH_CLI_YES=1`. Mutation only — there is deliberately no environment
/// variable that authorises a send.
fn env_yes() -> bool {
    matches!(
        std::env::var("MACH_CLI_YES").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

fn present(globals: &Globals, answer: &Value) -> String {
    if globals.json {
        return answer.to_string();
    }
    render::outcome(
        answer.get("summary").and_then(Value::as_str).unwrap_or(""),
        answer.get("payload").unwrap_or(&Value::Null),
    )
}

// ===========================================================================
// saying what there is
// ===========================================================================

fn list_tools(data_dir: &PathBuf, globals: &Globals) -> Result<String, CliError> {
    let (verbs, client) = surface(data_dir);
    if globals.json {
        let items: Vec<Value> = verbs
            .iter()
            .map(|v| {
                json!({
                    "name": v.name,
                    "description": v.description,
                    "inputSchema": v.schema,
                    "consent": v.consent.as_str(),
                    "local": v.local,
                })
            })
            .collect();
        return Ok(json!({ "ok": true, "tools": items, "appRunning": client.is_some() }).to_string());
    }

    let width = verbs.iter().map(|v| v.name.len()).max().unwrap_or(0);
    let mut out = String::new();
    for verb in &verbs {
        let mark = match (verb.consent, verb.local) {
            (Class::Read, true) => "",
            (Class::Read, false) => "  (app)",
            (Class::Mutate, _) => "  --yes",
            (Class::Outbound, _) => "  --yes --to…",
        };
        out.push_str(&format!(
            "{:<width$}  {}{mark}\n",
            verb.name,
            first_sentence(&verb.description),
        ));
    }
    if client.is_none() {
        out.push_str(
            "\nMach is not running, so this is the built-in surface: any installed \
             plugin's verbs are missing from it.\n",
        );
    }
    Ok(out)
}

fn help_for(data_dir: &PathBuf, globals: &Globals, typed: &str) -> Result<String, CliError> {
    if RESERVED.contains(&typed) {
        return usage(data_dir, globals);
    }
    let (verbs, _) = surface(data_dir);
    let names: Vec<String> = verbs.iter().map(|v| v.name.clone()).collect();
    let name = args::resolve_verb(typed, &names)?;
    let verb = verbs.iter().find(|v| v.name == name).expect("just resolved");

    if globals.json {
        return Ok(json!({
            "ok": true,
            "name": verb.name,
            "description": verb.description,
            "inputSchema": verb.schema,
            "consent": verb.consent.as_str(),
            "local": verb.local,
        })
        .to_string());
    }

    let mut out = format!("{}\n\n{}\n", verb.name, verb.description);
    let empty = serde_json::Map::new();
    let properties = verb
        .schema
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let required: Vec<&str> = verb
        .schema
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    if !properties.is_empty() {
        out.push('\n');
        let width = properties.keys().map(String::len).max().unwrap_or(0) + 2;
        for (key, property) in properties {
            let flag = format!("--{key}");
            let kind = property.get("type").and_then(Value::as_str).unwrap_or("string");
            let need = match required.contains(&key.as_str()) {
                true => "required",
                false => "optional",
            };
            let about = property
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default();
            out.push_str(&format!("{flag:<width$}  {kind}, {need}  {about}\n"));
        }
    }

    out.push('\n');
    out.push_str(match verb.consent {
        Class::Read => "Reads. Runs with or without the app.",
        Class::Mutate => "Changes the mailbox. Needs --yes, and needs Mach running.",
        Class::Outbound => {
            "Sends. Needs --yes, needs every recipient named with --to, and needs Mach running."
        }
    });
    out.push('\n');
    Ok(out)
}

fn usage(data_dir: &PathBuf, globals: &Globals) -> Result<String, CliError> {
    if globals.json {
        return list_tools(data_dir, globals);
    }
    Ok(format!(
        "mach — Mach's command layer, from a shell.\n\
         \n\
         \x20 mach <verb> [arguments]      run one of Mach's tools\n\
         \x20 mach tools                   every verb, and what each one costs\n\
         \x20 mach help <verb>             one verb's parameters\n\
         \x20 mach where                   the store, and whether the app is up\n\
         \n\
         \x20 --json                       machine-readable output, errors included\n\
         \x20 --yes                        authorise a change to the mailbox\n\
         \x20 --to <address>               name a recipient; sending needs all of them\n\
         \n\
         Reads run against {} whether or not Mach is open. Everything else needs\n\
         the app, and fails rather than queueing when it is not there.\n",
        crate::config::database_path(data_dir).display()
    ))
}

fn where_things_are(data_dir: &PathBuf, globals: &Globals) -> String {
    let store = crate::config::database_path(data_dir);
    let door = endpoint::path_in(data_dir);
    let running = Client::locate(data_dir.clone());

    if globals.json {
        return json!({
            "ok": true,
            "dataDir": data_dir.to_string_lossy(),
            "store": store.to_string_lossy(),
            "storeExists": store.exists(),
            "door": door.to_string_lossy(),
            "appRunning": running.is_ok(),
            "why": running.as_ref().err().map(|e| e.message.clone()),
        })
        .to_string();
    }

    let mut out = format!(
        "data      {}\nstore     {}{}\ndoor      {}\n",
        data_dir.display(),
        store.display(),
        match store.exists() {
            true => "",
            false => "  (not there)",
        },
        door.display(),
    );
    // The sentence, not a prefix and then the sentence: `Client::locate` already
    // says "Mach is not running" and which of the three reasons it was.
    out.push_str(&match running {
        Ok(_) => "app       running\n".to_string(),
        Err(e) => format!("app       {}\n", e.message),
    });
    out
}

/// The first sentence of a tool description, for a one-line listing.
fn first_sentence(description: &str) -> String {
    let flat = description.replace(['\n', '\r'], " ");
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.find(". ") {
        Some(stop) => flat[..=stop].to_string(),
        None => flat,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `mach help` must keep meaning `mach help`, whatever gets added to the
    /// surface — including by a plugin, whose tool names this codebase does not
    /// control.
    #[test]
    fn no_tool_is_named_after_a_reserved_word() {
        for tool in tools::tools() {
            assert!(
                !RESERVED.contains(&tool.definition.name.as_str()),
                "{} collides with a reserved word",
                tool.definition.name
            );
        }
    }

    #[test]
    fn a_description_shortens_to_its_first_sentence() {
        assert_eq!(
            first_sentence("List conversations. Use this for the inbox."),
            "List conversations."
        );
        assert_eq!(first_sentence("One line"), "One line");
    }
}
