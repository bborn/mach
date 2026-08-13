# Agent backends

⌘K, a sentence, and something has to think about it. That something is a
**backend**, and Mach has three. The default needs no configuration at all.

| backend | what it runs | credential |
|---|---|---|
| `claudeCli` | the Claude Code CLI, headless | none — whatever `claude` already uses |
| `anthropicApi` | `POST /v1/messages` directly | `ANTHROPIC_API_KEY` |
| `command` | a program you name | yours |

## Detection

Preferences → Agent → **Runs on** defaults to *Automatic*, which resolves in
this order, every time a session starts:

1. a `claude` executable — checked in `MACH_CLAUDE_BIN`, then `PATH`, then
   `~/.local/bin`, `~/.claude/local`, `/opt/homebrew/bin`, `/usr/local/bin`.
   The extra locations matter: an app launched from Finder inherits launchd's
   `PATH`, which contains no developer tooling;
2. `ANTHROPIC_API_KEY` (or `ANTHROPIC_AUTH_TOKEN`) in the environment, or in
   `.env.local` in a development build;
3. neither, in which case ⌘K says so and names both remedies.

Claude Code wins when both are available: the subscription has already paid for
that model, and an API key would charge for it twice.

Choosing a backend explicitly turns off the fallback. Asking for Claude Code on
a machine that has none is an error with a sentence, not a silent switch to
something you did not choose to spend.

**Model** is free text — `opus`, `sonnet`, a full model id, or empty for the
backend's own default. The two backends do not agree on what a model is called,
so there is no list to pick from.

## Written replies use the same backend

Preferences → Agent → **Replies** writes a reply for qualifying inbound mail,
and it resolves a backend exactly as ⌘K does. Two differences, both about
money:

- it ignores **Model** and uses **Reply model** instead, which defaults to
  `claude-sonnet-5`. It runs unattended against every human message addressed
  to you, so `opus` in the drawer must not become `opus` in your inbox;
- `command` cannot do it. That contract is a session — a tool server, a stream
  of events, an approval round trip — and a reply is one prompt and one string
  back. Preferences says so under the toggle, and the log says so when a pass
  would have written something.

On Claude Code it is `claude --print` with `--tools ""`, no MCP server, and
`--no-session-persistence`. The prompt goes on stdin. One generation takes
about seven seconds and, at list prices, a little over a cent; on a
subscription it comes out of the plan.

## How a backend reaches your mail

It doesn't. It reaches *Mach*, and Mach reaches your mail.

```
   backend  ──tool call──►  ToolGate  ──►  CommandDispatcher ──► Gmail / Calendar
  (in or out of process)       │            (typed · undoable · logged)
                               └──►  the owner, for anything that leaves the building
```

The tool surface is [the command catalogue](../src-tauri/src/commands/), plus
the local reads, plus the composer, plus any installed plugins — generated from
the catalogue, never hand-written per backend. There is no file access, no shell,
and no second path to Google. A backend cannot widen that surface: a call for a
tool that is not on it is refused before it is looked at.

Two rules are enforced in Mach's own process, for every backend equally:

- **What touches another human waits for you.** `send_draft`, `rsvp`, and every
  calendar write that can carry guests park the session and show you the
  sentence they would carry out. Everything else runs unattended, because the
  command layer records its own inverse and ⌘Z undoes it.
- **Nothing else can approve on your behalf.** The gate is on this side of the
  call. A CLI's own permission system — including
  `--dangerously-skip-permissions`, which Mach never passes — decides whether
  *it* may ask; it does not decide whether anything happens.

`src-tauri/tests/agent_backends.rs` pins this: a backend that calls `send_draft`
straight down the socket, with no model and no prompt of its own, still parks on
the owner, and a denial leaves the outbox empty.

## The tool server (MCP)

An out-of-process backend gets the tools over the [Model Context
Protocol](https://modelcontextprotocol.io). Mach serves it **inside the app**,
because the tools are this process's state — an open database, a dispatcher
holding the OAuth tokens, an outbox with a recall timer, a window that can ask
you a question. A stdio MCP server would be a second process holding none of
that.

That means a local port that can archive mail and send email, so:

- it binds `127.0.0.1` on an **ephemeral port**, never `0.0.0.0`;
- every request must carry `Authorization: Bearer <token>`, where the token is
  32 bytes from `/dev/urandom`, **minted per session** and compared in constant
  time. No token, no answer — not even `initialize`;
- the token is written to a `0600` file, never to a command line, and the file
  is deleted when the session ends;
- a request carrying an `Origin` header is refused outright: no browser has any
  business here, and that closes the DNS-rebinding case;
- the listener dies with the session. There is no long-lived local service.

Implemented methods: `initialize`, `ping`, `tools/list`, `tools/call`. Anything
else gets `-32601`.

## What the Claude Code backend is allowed to do

Mach starts it with, in effect:

```
claude --print --verbose --output-format stream-json --include-partial-messages
       --system-prompt <Mach's prompt>
       --tools ""                       # no Bash, Read, Write, WebFetch — none of it
       --mcp-config <0600 file>         # only Mach's tools
       --strict-mcp-config              # your own MCP servers are not loaded
       --setting-sources ""             # no hooks, no memory files, no output styles
       --disable-slash-commands
       --allowedTools mcp__mach         # Mach's tools may run; Mach does the asking
       [--model …] [--resume …]
```

It runs in `agent/` beside Mach's database — not your home directory, not a
repository — and the prompt goes in on stdin, not in an argument vector.

Authentication is the CLI's own business: the child inherits your environment and
authenticates exactly as `claude` does in your terminal.

One process per message; follow-ups `--resume` the session id the first run
reported, so the conversation keeps its history.

**This is not a coding agent.** Doing developer work on a real repository —
"look at this error and fix it" — is a different surface, deliberately, and it
belongs in your own Claude Code session with your own tools. The narrowness here
is what makes an agent inside a mail client safe to leave switched on.

## Writing your own backend

Set **Runs on** to *Custom command* and give the command line. Mach runs it once
per message:

| channel | what it carries |
|---|---|
| stdin | the message: the `<context>` block, the conversation so far on a follow-up, and your sentence. Closed after writing. |
| `MACH_SYSTEM_PROMPT` | who you are, what time it is, whose mail this is |
| `MACH_MCP_CONFIG` | path to an MCP config file — `{"mcpServers":{"mach":{"type":"http",…}}}` |
| `MACH_MCP_URL`, `MACH_MCP_TOKEN` | the same server, for a program that speaks MCP itself |
| `MACH_SESSION_ID` | stable across the messages of one session |
| cwd | a scratch directory owned by Mach |
| stdout | **the answer**, plain text, streamed to the drawer as it arrives |
| stderr | diagnostics, shown only if the program fails |
| exit code | `0` is an answer; anything else fails the session with the first line of stderr |

There is no protocol to implement. The whole of a working backend:

```sh
#!/bin/sh
exec claude --print --mcp-config "$MACH_MCP_CONFIG" --strict-mcp-config \
            --tools "" --allowedTools mcp__mach \
            --system-prompt "$MACH_SYSTEM_PROMPT"
```

A program that ignores the MCP server entirely is a legal backend too. It will
be an agent that can talk about your mail but not act on it — an honest outcome,
and one you can see in the drawer.

The command line is split on whitespace with quotes honoured; it is not a shell
fragment. No pipes, no `&&`, no variable expansion — a backend is a program, and
"which program is the agent" should have exactly one answer.

## Where things live

| file | what |
|---|---|
| `src-tauri/src/agent/backend.rs` | detection, preferences, the sentence when nothing is available |
| `src-tauri/src/agent/brain.rs` | the seam: what a backend is handed |
| `src-tauri/src/agent/cli.rs` | Claude Code |
| `src-tauri/src/agent/anthropic.rs` | the Messages API |
| `src-tauri/src/agent/command.rs` | your program |
| `src-tauri/src/agent/gate.rs` | the tool surface and the approval rule |
| `src-tauri/src/agent/mcp.rs` | the tool server |
| `src-tauri/tests/agent_backends.rs` | the tests that pin all of the above |
