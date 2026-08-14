import { defineConfig, type HotPayload, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import path from "node:path";
import {
  HELD_EVENT,
  HELLO_EVENT,
  TAKE_EVENT,
  TAKE_PAYLOAD,
  classify,
  isHeldKind,
  type HeldPayload,
} from "./src/lib/hmr-hold";

const host = process.env.TAURI_DEV_HOST;

/**
 * Hold hot updates until the window asks for them.
 *
 * `apply: "serve"` — there is no dev server in a production build, so there is
 * nothing to hold and nothing of this in the output. The window's half is
 * behind `import.meta.env.DEV` for the same reason; see `src/lib/hmr-hold.ts`
 * for why the interception is here rather than in a `vite:beforeFullReload`
 * listener, and for what it cannot catch.
 *
 * One wrapper around one method. `environment.hot.send` is where every
 * server-initiated update and reload in Vite 7 ends up, so it is the only place
 * that has to be taught to wait.
 */
function holdUpdates(): Plugin {
  return {
    name: "mach:hold-updates",
    apply: "serve",

    configureServer(server) {
      const channel = server.environments.client.hot;
      /** The real one, kept before the wrapper goes on. Nothing else may send. */
      const send = channel.send.bind(channel) as (payload: HotPayload) => void;

      let waiting = false;
      const announce = () =>
        send({ type: "custom", event: HELD_EVENT, data: { waiting } });

      channel.send = ((...args: unknown[]) => {
        // The other overload — `send(event, data)` for custom messages, ours
        // included. Nothing to decide about those.
        const [payload] = args;
        if (typeof payload === "string" || !isHeldKind(payload as { type: string })) {
          return send(...(args as [HotPayload]));
        }

        const { send: now, keep } = classify(payload as HeldPayload);
        if (now) send(now as HotPayload);
        if (!keep) return;

        waiting = true;
        announce();
      }) as typeof channel.send;

      /*
       * A window that has just loaded is running the newest code by definition,
       * so whatever was being held for its predecessor is already in it.
       *
       * With two windows open on one server this clears the other one's offer
       * too, and the offer is the only thing lost — the second window keeps
       * running what it has, and the next edit tells it so again.
       */
      channel.on(HELLO_EVENT, () => {
        waiting = false;
        announce();
      });

      channel.on(TAKE_EVENT, () => {
        if (!waiting) return;
        waiting = false;
        // The window's own `HELLO` clears the flag again on the other side of
        // this, so nothing needs to survive the navigation.
        send(TAKE_PAYLOAD as HotPayload);
      });
    },
  };
}

// https://vitejs.dev/config/
export default defineConfig(async () => ({
  plugins: [react(), tailwindcss(), holdUpdates()],

  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },

  // Vite options tailored for Tauri development
  //
  // 1. prevent vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell vite to ignore watching `src-tauri`
      //
      // ...and `.claude/`, which is where the agent harness puts a git worktree
      // per task — thirty-odd full checkouts of this repo, nested inside it.
      // Each one has a `tsconfig.json`, and vite answers a changed tsconfig
      // anywhere under the root by clearing its cache and forcing a full
      // reload. Deleting a merged worktree therefore reloaded the running app
      // against a tree that had just stopped existing, and it came back blank.
      ignored: ["**/src-tauri/**", "**/.claude/**"],
    },
  },

  // The dependency scanner crawls every `**/*.html` under the root looking for
  // entry points, which finds each worktree's `index.html` and follows it into
  // that branch's `src/`. This project has one entry point.
  optimizeDeps: {
    entries: ["index.html"],
  },
}));
