/**
 * A real, tiny plugin — the shape `docs/plugins.md` describes, cut down far
 * enough to fit on one screen.
 *
 * It exists to prove two things the canary cannot: that the boundary is not so
 * tight that useful work is impossible, and that a capability the manifest did
 * not declare is refused with a sentence rather than silently working.
 *
 * Its manifest declares `read: ["labels"]` and `commands: ["label"]`. The last
 * step deliberately asks for `trash`, which it did not declare.
 */

export const actions = {
  async file({ mach, threadIds }) {
    const steps = [];

    const labels = await mach.read.labels();
    steps.push(`read ${labels.length} labels`);

    const target = labels.find((l) => l.kind === "user");
    const result = await mach.run({
      kind: "label",
      threadIds,
      labelId: target.id,
      add: true,
    });
    steps.push(`ran label → ${result.message}`);

    // Not in the manifest. The host must refuse this, and the refusal must name
    // the missing capability rather than looking like an internal error.
    try {
      await mach.run({ kind: "trash", threadIds });
      steps.push("ran trash — THE HOST FAILED TO REFUSE THIS");
    } catch (error) {
      steps.push(`trash refused: ${error.message}`);
    }

    return steps;
  },
};

export const views = {
  summary({ threadIds }) {
    return {
      type: "section",
      title: "Example",
      children: [
        { type: "row", label: "Selected", value: String(threadIds.length) },
        { type: "button", label: "File it", action: "file" },
      ],
    };
  },
};
