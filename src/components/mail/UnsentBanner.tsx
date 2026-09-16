import { MailWarning } from "lucide-react";
import { useMach } from "@/hooks/useMach";
import { Kbd } from "@/components/ui/kbd";
import { useUnsent } from "./use-unsent";

/**
 * The top of the window, while anything he sent has not gone.
 *
 * The rail's Unsent row existed and was not enough: it is one line among the
 * mailboxes, and it only counted refusals. Seven replies once sat in the queue
 * for a day, drawn as sent in their conversations, and nothing on screen was
 * different. A message he believes is delivered and is not is the most
 * expensive thing this app can get wrong, so it takes the full width of the
 * window and stays until it is dealt with. It opens the same panel as `g u`.
 */
export function UnsentBanner() {
  const { actions } = useMach();
  const { failed } = useUnsent();
  if (failed.length === 0) return null;
  const count = failed.length;

  return (
    <button
      type="button"
      data-unsent-banner=""
      onClick={() => actions.setUnsent(true)}
      className="flex h-8 w-full shrink-0 items-center gap-2 bg-danger px-3 text-left text-list font-medium text-white"
    >
      <MailWarning size={14} strokeWidth={2} className="shrink-0" />
      <span className="min-w-0 truncate">
        {count === 1 ? "1 message not sent" : `${count} messages not sent`}
      </span>
      <span className="ml-auto flex shrink-0 items-center gap-1.5 text-micro">
        <Kbd keys="g u" className="border-white/40 bg-white/15 text-white" />
        review
      </span>
    </button>
  );
}
