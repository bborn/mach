import { defineConfig } from "vitest/config";
import path from "node:path";

export default defineConfig({
  resolve: {
    alias: { "@": path.resolve(__dirname, "./src") },
  },
  test: {
    environment: "node",
    include: ["src/**/*.{test,spec}.{ts,tsx}"],
    exclude: [
      "**/node_modules/**",
      "**/dist/**",
      // Each agent task is checked out into a worktree *inside* the repo, so a
      // default glob walks into another agent's half-finished branch and
      // reports its failures as ours. Scope to src/ and skip these explicitly.
      // `.claude/worktrees/` is where the harness puts them now;
      // `.task-worktrees/` was the old path.
      "**/.claude/**",
      "**/.task-worktrees/**",
      "**/.qa/**",
    ],
  },
});
