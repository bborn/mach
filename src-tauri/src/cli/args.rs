//! Turning a command line into a tool call, using the tool's own schema.
//!
//! # Nothing here knows what a verb is
//!
//! There is no table of commands in this file, and there must never be one. The
//! verbs are whatever [`ToolGate::tools`] currently says they are, and the
//! arguments each one takes are whatever its `inputSchema` says — the same
//! schema the model is handed, generated from
//! [`Command::catalogue`](crate::commands::Command::catalogue) for the command
//! half of the surface. So `mach` gains a verb when the app gains a tool, and
//! it gains a flag when a [`ParamSpec`](crate::commands::ParamSpec) gains a
//! field, with nothing recompiled here.
//!
//! That is also why the flags are `--draftId` rather than a friendlier
//! `--draft`: the flag *is* the schema property. [`resolve_flag`] accepts
//! `--draft-id` and `--draftid` for the same property, because a shell is a
//! place where nobody wants to hold down shift, but the name it resolves to is
//! the one the app published.
//!
//! # Verbs are resolved, not aliased
//!
//! `mach search invoice` works, and it works without an alias table: `search`
//! is an unambiguous prefix of `search_threads`. [`resolve_verb`] tries exact,
//! then exact-ignoring-punctuation, then unique prefix, and then gives up and
//! lists the candidates. An alias table would be the beginning of the parallel
//! vocabulary this whole design exists to avoid — the moment `mach inbox` is a
//! word, it is a word that has to be kept in step with `list_threads` by hand.
//!
//! [`ToolGate::tools`]: crate::ipc::agent::engine::gate::ToolGate::tools

use serde_json::{Map, Value};

use super::error::CliError;

// ===========================================================================
// the global half
// ===========================================================================

/// The flags that mean the same thing whatever verb they are next to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Globals {
    pub json: bool,
    pub yes: bool,
    pub help: bool,
}

/// Pull the global flags out of an argument list, leaving everything else in
/// order.
///
/// They are recognised anywhere rather than only before the verb, because
/// `mach archive 42 --yes` is what people type and refusing it would teach
/// nothing except that this tool is fussy.
pub fn take_globals(argv: &[String]) -> (Globals, Vec<String>) {
    let mut globals = Globals::default();
    let mut rest = Vec::new();
    for arg in argv {
        match arg.as_str() {
            "--json" => globals.json = true,
            "--yes" | "-y" => globals.yes = true,
            "--help" | "-h" => globals.help = true,
            _ => rest.push(arg.clone()),
        }
    }
    (globals, rest)
}

/// `--to alex@example.com`, repeated, pulled out of the rest.
///
/// Only called for [`Class::Outbound`](super::protocol::Class::Outbound) verbs,
/// where `--to` is the recipient confirmation rather than a parameter. Every
/// other verb — `draft_message` takes a `to` array of its own — sees `--to`
/// as an ordinary flag and it never reaches this function. `send_draft`'s
/// schema has no `to` property, so there is nothing to collide with, and
/// `the_send_verb_has_no_to_parameter_to_collide_with` pins that.
pub fn take_recipients(argv: &[String]) -> Result<(Vec<String>, Vec<String>), CliError> {
    let mut recipients = Vec::new();
    let mut rest = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        let arg = &argv[i];
        if let Some(value) = arg.strip_prefix("--to=") {
            recipients.push(value.to_string());
        } else if arg == "--to" {
            let value = argv.get(i + 1).ok_or_else(|| {
                CliError::new("usage", "--to needs an address after it")
            })?;
            recipients.push(value.clone());
            i += 1;
        } else {
            rest.push(arg.clone());
        }
        i += 1;
    }
    Ok((recipients, rest))
}

// ===========================================================================
// verbs
// ===========================================================================

/// Punctuation-insensitive, case-insensitive form of a name, for matching only.
fn normalise(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Which tool the operator meant.
///
/// Exact first, so a name that is a prefix of another (`get_event` and nothing
/// else starts with it, but the principle holds) always resolves to itself.
pub fn resolve_verb(typed: &str, names: &[String]) -> Result<String, CliError> {
    if let Some(exact) = names.iter().find(|n| n.as_str() == typed) {
        return Ok(exact.clone());
    }
    let wanted = normalise(typed);
    let same: Vec<&String> = names.iter().filter(|n| normalise(n) == wanted).collect();
    if let [only] = same.as_slice() {
        return Ok((*only).clone());
    }
    let prefixed: Vec<&String> = names
        .iter()
        .filter(|n| normalise(n).starts_with(&wanted))
        .collect();
    match prefixed.as_slice() {
        [only] => Ok((*only).clone()),
        [] => Err(CliError::new(
            "unknownVerb",
            format!("{typed} is not one of Mach's verbs. `mach tools` lists them."),
        )),
        many => {
            let mut candidates: Vec<&str> = many.iter().map(|n| n.as_str()).collect();
            candidates.sort_unstable();
            Err(CliError::new(
                "unknownVerb",
                format!("{typed} could be {}. Say which.", candidates.join(", ")),
            ))
        }
    }
}

// ===========================================================================
// arguments
// ===========================================================================

/// Which schema property a flag names.
///
/// `--unread-only`, `--unreadonly` and `--unreadOnly` are the same property;
/// nothing else is. An unknown flag is an error rather than being passed
/// through, because every schema in the surface is `additionalProperties:
/// false` and a typo silently dropped is a call that does something other than
/// what was typed.
fn resolve_flag(typed: &str, properties: &Map<String, Value>) -> Result<String, CliError> {
    if properties.contains_key(typed) {
        return Ok(typed.to_string());
    }
    let wanted = normalise(typed);
    let hit = properties.keys().find(|k| normalise(k) == wanted);
    match hit {
        Some(name) => Ok(name.clone()),
        None => {
            let mut known: Vec<&str> = properties.keys().map(String::as_str).collect();
            known.sort_unstable();
            Err(CliError::new(
                "usage",
                match known.is_empty() {
                    true => format!("--{typed} is not a parameter; this verb takes none"),
                    false => format!(
                        "--{typed} is not a parameter. This verb takes: {}",
                        known
                            .iter()
                            .map(|k| format!("--{k}"))
                            .collect::<Vec<_>>()
                            .join(" ")
                    ),
                },
            ))
        }
    }
}

fn type_of(property: &Value) -> &str {
    property
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("string")
}

/// One typed value out of one string, according to the schema.
fn coerce(name: &str, property: &Value, raw: &str) -> Result<Value, CliError> {
    let bad = |wanted: &str| {
        CliError::new(
            "usage",
            format!("--{name} takes {wanted}, and \u{201c}{raw}\u{201d} is not one"),
        )
    };
    Ok(match type_of(property) {
        "integer" => Value::from(raw.trim().parse::<i64>().map_err(|_| bad("a whole number"))?),
        "number" => Value::from(raw.trim().parse::<f64>().map_err(|_| bad("a number"))?),
        "boolean" => match raw.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "1" | "" => Value::Bool(true),
            "false" | "no" | "0" => Value::Bool(false),
            _ => return Err(bad("true or false")),
        },
        // A whole object has to arrive as JSON. There is no flag spelling for
        // `{ title, startTs, attendees: [...] }` that is not worse than the
        // JSON, and inventing one would be inventing a schema language.
        "object" => serde_json::from_str(raw).map_err(|e| {
            CliError::new("usage", format!("--{name} takes a JSON object: {e}"))
        })?,
        "array" => {
            let items = property.get("items").cloned().unwrap_or(Value::Null);
            let values: Result<Vec<Value>, CliError> = raw
                .split(',')
                .map(str::trim)
                .filter(|piece| !piece.is_empty())
                .map(|piece| coerce(name, &items, piece))
                .collect();
            Value::Array(values?)
        }
        _ => Value::String(raw.to_string()),
    })
}

/// Everything after the verb, as the object the tool takes.
///
/// Positionals fill the required properties in the order the schema lists them,
/// which is what makes `mach get_thread 4127` and `mach archive 1,2,3` work
/// without either being spelled out anywhere.
pub fn build_input(tokens: &[String], schema: &Value) -> Result<Value, CliError> {
    let empty = Map::new();
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);

    let mut input = Map::new();
    let mut positional: Vec<String> = Vec::new();

    let mut i = 0;
    while i < tokens.len() {
        let token = &tokens[i];
        let Some(flag) = token.strip_prefix("--") else {
            positional.push(token.clone());
            i += 1;
            continue;
        };

        let (typed, inline) = match flag.split_once('=') {
            Some((k, v)) => (k, Some(v.to_string())),
            None => (flag, None),
        };
        let name = resolve_flag(typed, properties)?;
        let property = properties.get(&name).cloned().unwrap_or(Value::Null);

        let raw = match inline {
            Some(value) => value,
            None => match type_of(&property) {
                // `--unreadOnly` on its own means yes, and that is the common
                // spelling. But a boolean schema means false is a real answer —
                // `star` documents "true stars, false unstars" — and `--starred
                // false` is how a person writes it. So the next token is taken
                // only when it is literally `true` or `false`.
                //
                // Only those two words, and pointedly not the aliases `1`, `0`,
                // `yes` and `no` that `coerce` accepts after an `=`. Thread ids
                // are integers, so `--starred 0` would be a genuine ambiguity
                // between a value and the first positional, and guessing wrong
                // there stars the wrong conversation. `--starred=0` stays
                // unambiguous and still works.
                "boolean" => match tokens.get(i + 1).map(String::as_str) {
                    Some(word @ ("true" | "false")) => {
                        i += 1;
                        word.to_string()
                    }
                    _ => String::new(),
                },
                _ => {
                    let next = tokens.get(i + 1).filter(|t| !t.starts_with("--"));
                    let value = next
                        .ok_or_else(|| {
                            CliError::new("usage", format!("--{name} needs a value after it"))
                        })?
                        .clone();
                    i += 1;
                    value
                }
            },
        };

        let value = coerce(&name, &property, &raw)?;
        // A repeated flag on an array property appends rather than replacing,
        // so `--addLabelIds A --addLabelIds B` and `--addLabelIds A,B` are the
        // same call.
        match (input.get_mut(&name), &value) {
            (Some(Value::Array(existing)), Value::Array(more)) => existing.extend(more.clone()),
            _ => {
                input.insert(name, value);
            }
        }
        i += 1;
    }

    // Positionals fill what is still required, in schema order.
    let required: Vec<String> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|r| {
            r.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let mut waiting = positional.into_iter();
    for name in required {
        if input.contains_key(&name) {
            continue;
        }
        let Some(raw) = waiting.next() else { continue };
        let property = properties.get(&name).cloned().unwrap_or(Value::Null);
        input.insert(name.clone(), coerce(&name, &property, &raw)?);
    }
    let leftover: Vec<String> = waiting.collect();
    if !leftover.is_empty() {
        // A stray `yes`/`no`/`1`/`0` is almost always somebody answering a
        // boolean the long way round, since those are the aliases `coerce`
        // takes after an `=` but the parser will not read off the next token
        // (see the boolean arm above). "nothing takes yes" reads as though the
        // value were wrong rather than the spelling, which is the report that
        // brought this here.
        let boolish = leftover
            .iter()
            .find(|word| matches!(word.to_ascii_lowercase().as_str(), "yes" | "no" | "1" | "0"));
        let hint = match boolish {
            Some(word) => format!(" If it is a yes-or-no answer, write it as --flag={word}."),
            None => String::new(),
        };
        return Err(CliError::new(
            "usage",
            format!(
                "nothing takes {}. Name it with a flag, or drop it.{hint}",
                leftover.join(" ")
            ),
        ));
    }

    Ok(Value::Object(input))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::agent::engine::tools;
    use serde_json::json;

    fn names() -> Vec<String> {
        tools::tools()
            .iter()
            .map(|t| t.definition.name.clone())
            .collect()
    }

    #[test]
    fn a_verb_resolves_by_prefix_and_says_so_when_it_cannot() {
        assert_eq!(resolve_verb("search_threads", &names()).unwrap(), "search_threads");
        // Unambiguous prefixes, with no alias table behind them.
        assert_eq!(resolve_verb("search", &names()).unwrap(), "search_threads");
        assert_eq!(resolve_verb("archi", &names()).unwrap(), "archive");
        // Punctuation is not the name.
        assert_eq!(resolve_verb("get-thread", &names()).unwrap(), "get_thread");

        // `list_threads`, `list_events`, `list_labels`… — the CLI must not pick
        // one for the operator.
        let ambiguous = resolve_verb("list", &names()).unwrap_err();
        assert_eq!(ambiguous.kind, "unknownVerb");
        assert!(ambiguous.message.contains("list_labels"), "{}", ambiguous.message);

        assert_eq!(resolve_verb("frobnicate", &names()).unwrap_err().kind, "unknownVerb");
    }

    #[test]
    fn globals_are_recognised_wherever_they_appear() {
        let argv: Vec<String> = ["archive", "42", "--yes", "--json"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (globals, rest) = take_globals(&argv);
        assert!(globals.yes && globals.json && !globals.help);
        assert_eq!(rest, vec!["archive".to_string(), "42".to_string()]);
    }

    #[test]
    fn a_positional_fills_the_one_required_parameter() {
        let schema = json!({
            "type": "object",
            "properties": { "threadId": { "type": "integer" } },
            "required": ["threadId"],
        });
        let tokens = vec!["4127".to_string()];
        assert_eq!(build_input(&tokens, &schema).unwrap(), json!({ "threadId": 4127 }));
    }

    /// `star` documents "true stars, false unstars", so false has to be
    /// sayable. It was not: the boolean arm never read the next token, so
    /// `--starred false` left `false` as a positional and the verb was told
    /// `true`, which is the opposite of the request.
    #[test]
    fn a_boolean_takes_the_word_after_it() {
        let schema = json!({
            "type": "object",
            "properties": {
                "starred": { "type": "boolean" },
                "threadIds": { "type": "array", "items": { "type": "integer" } },
            },
            "required": ["starred", "threadIds"],
        });
        let call = |args: &[&str]| {
            build_input(
                &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                &schema,
            )
        };
        for (args, starred) in [
            (vec!["29682", "--starred", "false"], false),
            (vec!["29682", "--starred", "true"], true),
            (vec!["29682", "--starred=false"], false),
            // The bare flag still means yes, which is the common spelling.
            (vec!["29682", "--starred"], true),
        ] {
            assert_eq!(
                call(&args).unwrap(),
                json!({ "starred": starred, "threadIds": [29682] }),
                "{args:?}"
            );
        }
    }

    /// Thread ids are integers, so reading `0` or `1` off as a value would be a
    /// coin flip between the answer and the first positional — and calling it
    /// wrong acts on the wrong conversation. Only the two unambiguous words are
    /// taken; the other spellings `coerce` accepts after an `=` are left where
    /// they fall, and the error names the form that works.
    #[test]
    fn a_boolean_leaves_its_other_spellings_alone_and_says_how_to_write_them() {
        let schema = json!({
            "type": "object",
            "properties": {
                "starred": { "type": "boolean" },
                "threadIds": { "type": "array", "items": { "type": "integer" } },
            },
            "required": ["starred", "threadIds"],
        });
        let args: Vec<String> = ["29682", "--starred", "yes"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let error = build_input(&args, &schema).unwrap_err();
        assert!(
            error.message.contains("--flag=yes"),
            "names the spelling it wants: {}",
            error.message
        );
        let fixed: Vec<String> = ["29682", "--starred=yes"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            build_input(&fixed, &schema).unwrap(),
            json!({ "starred": true, "threadIds": [29682] })
        );
    }

    #[test]
    fn a_batch_arrives_comma_separated_or_repeated() {
        let schema = json!({
            "type": "object",
            "properties": { "threadIds": { "type": "array", "items": { "type": "integer" } } },
            "required": ["threadIds"],
        });
        assert_eq!(
            build_input(&["1,2,3".to_string()], &schema).unwrap(),
            json!({ "threadIds": [1, 2, 3] })
        );
        let repeated: Vec<String> = ["--threadIds", "1", "--thread-ids", "2"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            build_input(&repeated, &schema).unwrap(),
            json!({ "threadIds": [1, 2] })
        );
    }

    #[test]
    fn a_bare_boolean_flag_means_yes_and_a_typo_is_refused() {
        let schema = json!({
            "type": "object",
            "properties": { "unreadOnly": { "type": "boolean" }, "labelId": { "type": "string" } },
        });
        assert_eq!(
            build_input(&["--unread-only".to_string()], &schema).unwrap(),
            json!({ "unreadOnly": true })
        );
        let typo = build_input(&["--unredOnly".to_string()], &schema).unwrap_err();
        assert_eq!(typo.kind, "usage");
        assert!(typo.message.contains("--unreadOnly"), "{}", typo.message);
    }

    #[test]
    fn a_word_that_is_not_a_number_is_refused_rather_than_sent() {
        let schema = json!({
            "type": "object",
            "properties": { "threadId": { "type": "integer" } },
            "required": ["threadId"],
        });
        assert_eq!(
            build_input(&["sometime".to_string()], &schema).unwrap_err().kind,
            "usage"
        );
    }

    /// `--to` is the recipient confirmation on a send and an ordinary parameter
    /// everywhere else. That is only unambiguous because the send verb has no
    /// `to` of its own, so the claim is asserted rather than assumed.
    #[test]
    fn the_send_verb_has_no_to_parameter_to_collide_with() {
        let send = tools::find(tools::SEND_TOOL).expect("send_draft is in the surface");
        let properties = send.definition.input_schema["properties"]
            .as_object()
            .expect("an object schema");
        assert!(
            !properties.contains_key("to"),
            "send_draft grew a `to` parameter; --to now means two things"
        );

        let argv: Vec<String> = ["--draftId", "d_1", "--to", "a@b.test", "--to=c@d.test"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (recipients, rest) = take_recipients(&argv).unwrap();
        assert_eq!(recipients, vec!["a@b.test".to_string(), "c@d.test".to_string()]);
        assert_eq!(rest, vec!["--draftId".to_string(), "d_1".to_string()]);
    }
}
