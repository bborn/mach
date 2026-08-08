/**
 * File the selected conversations to a label, then archive them.
 *
 * The label list is ranked by how often it has been used from here, which is the
 * whole reason this is nicer than the built-in label picker.
 */

const RECENTS_KEY = "recent-labels";
const RECENT_LIMIT = 8;

export const actions = {
  async file({ mach, threadIds }) {
    if (threadIds.length === 0) {
      mach.notify("Nothing selected");
      return;
    }

    const labels = await mach.read.labels();
    const recents = (await mach.store.get(RECENTS_KEY)) ?? [];

    // User labels only — filing to CHAT or to SENT is not a thing anyone means.
    const choices = labels
      .filter((l) => l.kind === "user")
      .sort((a, b) => rank(b, recents) - rank(a, recents) || a.name.localeCompare(b.name))
      .map((l) => ({ id: l.id, title: l.name, value: l.id }));

    const labelId = await mach.ask.pick({ title: "File to…", items: choices });
    if (labelId === null) return; // dismissed

    // Two commands, one undo group. The host takes care of that.
    await mach.run({ kind: "label", threadIds, labelId, add: true });
    await mach.run({ kind: "archive", threadIds });

    await mach.store.set(
      RECENTS_KEY,
      [labelId, ...recents.filter((id) => id !== labelId)].slice(0, RECENT_LIMIT),
    );

    const name = choices.find((c) => c.value === labelId)?.title ?? "label";
    mach.notify(`Filed ${threadIds.length} to ${name}`);
  },
};

/** Recency rank: 100 for the most recent, descending. 0 for never used. */
function rank(label, recents) {
  const i = recents.indexOf(label.id);
  return i === -1 ? 0 : 100 - i;
}
