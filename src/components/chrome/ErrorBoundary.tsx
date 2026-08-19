import { Component, type ErrorInfo, type ReactNode } from "react";

/**
 * The other half of the white window.
 *
 * `index.html` carries a watchdog for the app never arriving. This is for the
 * app arriving and then throwing: React unmounts the whole tree on an
 * unhandled render error, and with nothing catching that, `createRoot().render()`
 * leaves exactly the same blank page — no message, no console hint, nothing to
 * tell it apart from a dev server that died.
 *
 * That is the failure mode this project has said costs it the most time, in as
 * many words: *silent failure is the specific thing that has cost this project
 * the most time*. Every write to Google has to say when it is refused. A frontend
 * that vanishes should be held to the same rule.
 *
 * # Why it renders no component of ours
 *
 * Plain elements and inline styles, no `ui/` primitives, no tokens, no `cn`.
 * Whatever threw is somewhere in this app, and the one thing this boundary must
 * never do is throw while explaining that something threw. Its own imports are
 * React and nothing else, so the set of code that has to be working for it to
 * draw is as small as it can be.
 *
 * # Why it does not try to recover
 *
 * There is a "Reload" button and no "Try again". Re-rendering the subtree that
 * just threw will nearly always throw again — the state that caused it is still
 * there — and a button that appears to retry and silently does nothing is worse
 * than no button. A reload is the honest offer.
 */
interface Props {
  children: ReactNode;
}

interface State {
  error: Error | null;
  componentStack: string | null;
}

export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null, componentStack: null };

  static getDerivedStateFromError(error: Error): Partial<State> {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    // Console as well as screen. The screen is for the person looking at it;
    // the console is what `webqa console` and Safari's inspector can read
    // afterwards, and what an agent debugging this will reach for first.
    console.error("[mach] the interface crashed", error, info.componentStack);
    this.setState({ componentStack: info.componentStack ?? null });
  }

  render() {
    const { error, componentStack } = this.state;
    if (!error) return this.props.children;

    const report = [
      error.stack || `${error.name}: ${error.message}`,
      componentStack ? `\nComponent stack:${componentStack}` : "",
    ].join("");

    return (
      <div
        data-mach-crashed
        style={{
          position: "fixed",
          inset: 0,
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          padding: "2rem",
          font: "13px/1.55 ui-sans-serif, -apple-system, system-ui, sans-serif",
          background: "Canvas",
          color: "CanvasText",
          WebkitUserSelect: "text",
          userSelect: "text",
        }}
      >
        <div style={{ maxWidth: "44rem", width: "100%" }}>
          <div style={{ fontSize: 15, fontWeight: 600, marginBottom: ".4rem" }}>
            Mach stopped
          </div>
          <div style={{ opacity: 0.7 }}>
            Nothing has been lost — the mail is on disk, and reloading picks it back up.
          </div>

          <pre
            style={{
              marginTop: "1rem",
              maxHeight: "22rem",
              overflow: "auto",
              padding: ".75rem .9rem",
              borderRadius: 6,
              border: "1px solid color-mix(in oklab, CanvasText 20%, transparent)",
              font: "12px/1.5 ui-monospace, SFMono-Regular, Menlo, monospace",
              whiteSpace: "pre-wrap",
              wordBreak: "break-word",
            }}
          >
            {report}
          </pre>

          <div style={{ marginTop: "1.1rem", display: "flex", gap: ".5rem" }}>
            <button type="button" onClick={() => location.reload()} style={BUTTON}>
              Reload
            </button>
            <button
              type="button"
              onClick={() => void navigator.clipboard?.writeText(report)}
              style={BUTTON}
            >
              Copy the error
            </button>
          </div>
        </div>
      </div>
    );
  }
}

const BUTTON: React.CSSProperties = {
  font: "inherit",
  padding: ".35rem .9rem",
  borderRadius: 6,
  border: "1px solid color-mix(in oklab, CanvasText 35%, transparent)",
  background: "transparent",
  color: "inherit",
};
