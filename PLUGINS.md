# Plugins

**Listing here is not an endorsement.** Plugins are third-party code, written by
people the maintainer has not met and does not vouch for. The maintainer reviews
the row, not the code. The capabilities column is what you are actually agreeing
to when you install one — read it, and read the install prompt, which says the
same thing in sentences.

A tier-1 plugin runs in a sandbox with **no network, no access to the app, no
access to your mail beyond what it declared, and no Google credentials, ever.**
It acts only by dispatching the same commands your keyboard dispatches, so
everything it does is undoable with ⌘Z and is recorded against its name. The
worst a compromised one can do is misuse the commands you granted it, at a
limited rate. That is a bad afternoon, not a breach.

`mach --safe-mode` boots with every plugin disabled, without uninstalling
anything.

## Reference plugins

These two ship in this repository, under `plugins/`, as the worked examples in
[`docs/plugins.md`](docs/plugins.md).

| Plugin | What it does | Capabilities |
|---|---|---|
| [Quick File](plugins/quick-file) | Pick a label, apply it to the selection, archive it — one keystroke, one undo | `labels` read, `label`, `archive` |
| [Snooze Until Free](plugins/snooze-until-free) | Snoozes to the next real gap in your calendar, and says when that is before you press the key | `calendar` read, `snooze` |

## Community plugins

Open a pull request adding one row. That is the entire submission process: no
registry, no account, no build service, no review queue.

```markdown
| [Your Plugin](https://github.com/you/mach-your-plugin) | One line about what it does | `calendar` read, `snooze` |
```

| Plugin | What it does | Capabilities |
|---|---|---|
| — | — | — |

## What the maintainer checks before merging a row

Three things, none of which is "is this code safe":

1. The repository has a `mach-plugin.json` at its root and it parses.
2. The capabilities column matches the manifest.
3. The one-line description is honest about what the plugin does.

## What you should check before installing one

- **The install prompt, in full.** It is written to be read. If a line surprises
  you, close it.
- **`read: ["threads"]` means it can read your mail** — the actual text of your
  messages. `read: ["threads.metadata"]` cannot. A plugin that only needs to
  know who wrote to you should be asking for the second one.
- **Anything that says it runs outside the sandbox.** No amount of manifest
  declaration makes an outbound socket safe. If a plugin can reach a server, it
  can send that server anything it can read, and the only real control is
  whether you trust the author.
- **Updates are not automatic, and a plugin that starts asking for more stops
  and shows you the difference.** That is a real protection against the update
  that turns malicious, but it is not a guarantee; the guarantee is that a
  tier-1 plugin has nowhere to send anything.

## No badges

There is no verified-publisher badge and there will not be one. A badge that can
be bought with a domain registration launders trust the maintainer never
extended, which is what happened to VS Code's when the Darcula incident used it
as an attack tool.
