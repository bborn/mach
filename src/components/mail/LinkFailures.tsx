import { useEffect } from "react";
import { useMach } from "@/hooks/useMach";
import { subscribeLinkFailures } from "@/lib/message-body";

/**
 * A link that could not be opened, on screen.
 *
 * Renders nothing. It exists because the thing that knows a link failed is
 * never the thing that can say so: opening happens in Rust, at the navigation
 * layer, with no promise to reject and no component in scope. Without this the
 * app's answer to "that did nothing" is still nothing — which is the specific
 * failure that has cost this project the most time, and the reason the same
 * dead-link report arrived twice.
 *
 * Mounted once, by `App`, rather than per message: the failure can outlive the
 * frame that caused it, and one subscription cannot report the same failure
 * three times because three messages happen to be expanded.
 */
export function LinkFailures() {
  const { actions } = useMach();

  useEffect(
    () => subscribeLinkFailures((message) => actions.setStatus(message, "error")),
    [actions],
  );

  return null;
}
