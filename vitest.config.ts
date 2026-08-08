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
      // TaskYou checks each task out into a worktree *inside* the repo, so a
      // default glob walks into another agent's half-finished branch and
      // reports its failures as ours. Scope to src/ and skip these explicitly.
      "**/.task-worktrees/**",
      "**/.qa/**",
    ],
  },
});
