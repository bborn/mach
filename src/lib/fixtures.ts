/**
 * Fixture data, shaped exactly like what the Rust command layer will return.
 *
 * Nothing here should be imported by a component. Components go through
 * `src/lib/data.ts`, which is the seam the real IPC source drops into.
 *
 * Times are generated relative to "now" so the week grid always has a
 * populated current week and the thread list always has fresh mail.
 */

import type {
  Account,
  Attachment,
  Calendar,
  CalendarEvent,
  Label,
  Message,
  Participant,
  Thread,
} from "@/types";
import { DAY, HOUR, MINUTE, addDays, startOfDay, startOfWeek } from "./time";

export const ME: Participant = { name: "Alex Rivera", email: "alex@northwind.example" };

/**
 * Ids are numbers, exactly like the SQLite row ids the real source returns.
 * The seed tables below still key accounts by `a1`…`a5` because that reads
 * better in a 44-row literal; `accountFor` is the one place it is resolved.
 */
export const accounts: Account[] = [
  { id: 1, email: "alex@northwind.example", name: "Northwind", colorIndex: 1, kind: "workspace" },
  { id: 2, email: "alex@lumen.example", name: "Lumen", colorIndex: 2, kind: "workspace" },
  { id: 3, email: "alex@talleres.example", name: "Talleres Ríos", colorIndex: 3, kind: "workspace" },
  { id: 4, email: "alex.rivera@example.com", name: "Personal", colorIndex: 4, kind: "personal" },
  { id: 5, email: "alex@meridian.example", name: "Meridian Data", colorIndex: 5, kind: "workspace" },
];

/** `a3` → the third account. Seed tables speak in keys, the data speaks in ids. */
function accountFor(key: string): Account {
  return accounts[Number(key.slice(1)) - 1]!;
}

export const labels: Label[] = [
  { id: "INBOX", accountId: null, name: "Inbox", kind: "system" },
  { id: "STARRED", accountId: null, name: "Starred", kind: "system" },
  { id: "SNOOZED", accountId: null, name: "Snoozed", kind: "system" },
  { id: "SENT", accountId: null, name: "Sent", kind: "system" },
  { id: "ARCHIVE", accountId: null, name: "Archive", kind: "system" },
  { id: "L_INVESTORS", accountId: null, name: "Investors", kind: "user" },
  { id: "L_CUSTOMERS", accountId: null, name: "Customers", kind: "user" },
  { id: "L_RECEIPTS", accountId: null, name: "Receipts", kind: "user" },
  { id: "L_FAMILY", accountId: null, name: "Family", kind: "user" },
];

/**
 * Calendars as `calendarList.list` describes them, not as ids.
 *
 * The colours and access roles are here rather than left off because they are
 * what the sidebar and the edit guard now read: without them the fixture app
 * exercises only the "Google never told us" path, which is the one branch that
 * was already working.
 */
export const calendars: Calendar[] = [
  {
    id: "c1",
    accountId: 1,
    name: "Northwind",
    colorIndex: 1,
    backgroundColor: "#3f51b5",
    accessRole: "owner",
    primary: true,
  },
  { id: "c2", accountId: 2, name: "Lumen", colorIndex: 2, backgroundColor: "#0b8043", accessRole: "owner" },
  {
    id: "c3",
    accountId: 3,
    name: "Talleres Ríos",
    colorIndex: 3,
    backgroundColor: "#f6bf26",
    accessRole: "writer",
    description: "Shop floor and deliveries",
  },
  { id: "c4", accountId: 4, name: "Family", colorIndex: 4, backgroundColor: "#d50000", accessRole: "owner" },
  {
    id: "c5",
    accountId: 5,
    name: "Meridian Data",
    colorIndex: 5,
    backgroundColor: "#8e24aa",
    // A calendar somebody shared read-only, so the editor is never offered on
    // it — the one state that is invisible until you try to drag something.
    accessRole: "reader",
  },
];

export const people: Participant[] = [
  { name: "Tawny Reeves", email: "tawny@northloop.example" },
  { name: "Priya Raghunathan", email: "priya@northwind.example" },
  { name: "Marcus Oyelaran", email: "marcus@lumen.example" },
  { name: "Dana Okonkwo", email: "dana@northwind.example" },
  { name: "Hana Kobayashi", email: "hana@shopify.com" },
  { name: "Deb Feldman", email: "deb@feldmanlegal.example" },
  { name: "Ilya Ostrovsky", email: "ilya@meridian.example" },
  { name: "Rosa Delgado", email: "rosa@talleres.example" },
  { name: "Sam Whitfield", email: "sam@stripe.com" },
  { name: "Riley Rivera", email: "riley@example.com" },
  { name: "Aurora Chen", email: "aurora@linear.app" },
  { name: "Google Cloud Billing", email: "billing-noreply@google.com" },
  { name: "Anthropic", email: "receipts@anthropic.com" },
  { name: "Cassidy Wren", email: "cassidy@paperbark.example" },
  { name: "Tomás Iglesias", email: "tomas@talleres.example" },
];

function person(email: string): Participant {
  return people.find((p) => p.email === email) ?? { name: email, email };
}

/** [accountId, fromEmail, subject, snippet, hoursAgo, unread, attachment, labels] */
type ThreadSeed = [
  string,
  string,
  string,
  string,
  number,
  boolean,
  boolean,
  string[],
];

const THREAD_SEEDS: ThreadSeed[] = [
  ["a1", "tawny@northloop.example", "Re: Series A data room — a few gaps", "The cohort tab still shows blended retention. Can you split paid vs organic before Thursday? Otherwise this looks", 0.4, true, true, ["INBOX", "L_INVESTORS"]],
  ["a2", "marcus@lumen.example", "Checkout conversion dropped 6% overnight", "Started around 02:40 UTC. Rolling back the address autocomplete change first since that's the only thing that", 1.1, true, false, ["INBOX"]],
  ["a1", "priya@northwind.example", "Q3 roadmap — final pass before we lock it", "I moved the Meta scope review ahead of the reporting rewrite. Two engineers freed up when the migration finished", 2.3, true, true, ["INBOX"]],
  ["a4", "riley@example.com", "basketball tryouts moved to thursday", "coach emailed everyone, its 4pm now not 5. can you still pick me up? also i need the physical form signed", 3.0, true, false, ["INBOX", "L_FAMILY"]],
  ["a1", "hana@shopify.com", "Partner listing review — one blocker", "Your OAuth consent copy references scopes you no longer request. Update the screenshot set and I can push this", 4.5, false, false, ["INBOX"]],
  ["a5", "ilya@meridian.example", "Warehouse costs, October", "Snowflake is 4.2k, up from 2.8k. Almost all of it is the hourly rollup job nobody reads. Proposing we drop it to", 5.2, true, true, ["INBOX"]],
  ["a3", "rosa@talleres.example", "Pedido #4482 — retraso del herraje", "El proveedor confirmó que el envío sale el martes. ¿Aviso yo al cliente o prefieres escribirle tú directamente", 6.1, false, false, ["INBOX", "L_CUSTOMERS"]],
  ["a1", "dana@northwind.example", "QA pass on the reporting rewrite", "Nine issues, three are blockers. The date picker resets to today when you change accounts, and CSV export drops", 7.4, false, true, ["INBOX"]],
  ["a2", "sam@stripe.com", "Your Stripe account: radar rule triggered 42 times", "A rule you created on Sep 4 blocked 42 payments in the last 24 hours, totalling $6,140. Review whether this is", 8.8, false, false, ["INBOX"]],
  ["a4", "deb@feldmanlegal.example", "Trust documents — signature needed", "Attached the restated version with the changes we discussed. Sign pages 14 and 22 only; the rest is exhibits", 11.0, true, true, ["INBOX", "L_FAMILY"]],
  ["a1", "aurora@linear.app", "Re: Bulk API rate limits", "We can raise you to 5k/hour on the current plan. Beyond that it's a conversation about the enterprise tier, but", 13.2, false, false, ["INBOX"]],
  ["a5", "receipts@anthropic.com", "Your Anthropic receipt — $1,284.30", "Invoice INV-2026-08-0442 for API usage between Jul 1 and Jul 31. Paid automatically with the card ending 4417", 14.5, false, true, ["INBOX", "L_RECEIPTS"]],
  ["a3", "tomas@talleres.example", "Fotos del showroom de Polanco", "Mandé al fotógrafo el jueves. Las de la cocina salieron mejor que las del vestidor, creo que por la luz de la", 16.0, false, true, ["INBOX"]],
  ["a2", "cassidy@paperbark.example", "Intro: Paperbark x Lumen", "Marcus suggested we talk. We're doing about 40k orders a month and our current offer engine is a pile of Shopify", 19.0, true, false, ["INBOX", "L_CUSTOMERS"]],
  ["a1", "billing-noreply@google.com", "Your Google Cloud invoice is available", "Account 0142-9987-3311. Amount due: $412.66. This invoice will be charged automatically on Aug 15", 22.0, false, false, ["INBOX", "L_RECEIPTS"]],
  ["a1", "tawny@northloop.example", "Thursday 2pm still works?", "I'll bring Priyanka from the platform team. She has questions about the creator payout flow that I couldn't", 26.0, false, false, ["INBOX", "L_INVESTORS"]],
  ["a2", "marcus@lumen.example", "Postmortem: the 6% checkout drop", "Root cause was the address autocomplete change — it silently failed for anyone with a non-US billing country", 28.0, false, true, ["INBOX"]],
  ["a1", "priya@northwind.example", "Hiring: two staff engineer candidates", "Both are strong. Candidate A is better on distributed systems, candidate B has actually shipped a sync engine", 31.0, false, false, ["INBOX"]],
  ["a4", "deb@feldmanlegal.example", "Re: Trust documents — signature needed", "No rush on the exhibits, but the signature pages should be back before the 20th or we redo the notary", 34.0, false, false, ["INBOX", "L_FAMILY"]],
  ["a5", "ilya@meridian.example", "dbt model naming — settling this", "Proposal: stg_ for sources, int_ for intermediate, fct_/dim_ for marts. No exceptions, including the legacy", 37.0, false, false, ["INBOX"]],
  ["a1", "hana@shopify.com", "Approved — listing goes live Monday", "Consent copy looks right now. Marketing will feature you in the app store roundup the week after, assuming the", 40.0, false, false, ["INBOX"]],
  ["a3", "rosa@talleres.example", "Nómina de agosto", "Adjunto el cálculo. Subieron las horas extra por el pedido de Santa Fe, revisa la línea de Tomás antes de que", 44.0, false, true, ["INBOX"]],
  ["a2", "sam@stripe.com", "Payout summary — $84,201.55", "Your payout of $84,201.55 is on its way and should arrive by Aug 8. This covers 1,842 charges", 48.0, false, false, ["INBOX", "L_RECEIPTS"]],
  ["a1", "dana@northwind.example", "Re: QA pass on the reporting rewrite", "Retested. Date picker is fixed, CSV export still drops the last row when the range ends on a Sunday", 52.0, false, false, ["INBOX"]],
  ["a4", "riley@example.com", "can i get the new headphones for my birthday", "the ones i showed you. theyre on sale until sunday which is technically before my birthday but the sale ends", 56.0, false, false, ["INBOX", "L_FAMILY"]],
  ["a1", "aurora@linear.app", "Changelog: sub-issues, finally", "You asked for this in 2024. Sub-issues ship today, along with a proper API for them and keyboard navigation", 61.0, false, false, ["INBOX"]],
  ["a5", "receipts@anthropic.com", "Usage alert: 80% of monthly budget", "Your organization has used 80% of the $2,000 monthly budget with 9 days remaining in the billing period", 66.0, false, false, ["INBOX"]],
  ["a2", "cassidy@paperbark.example", "Re: Intro: Paperbark x Lumen", "Tuesday works. Sending an invite — is 45 minutes enough or should I block an hour to include our RevOps lead", 70.0, false, false, ["INBOX", "L_CUSTOMERS"]],
  ["a1", "tawny@northloop.example", "Reference calls — three names", "Two customers and one former report. Happy to make the intros myself if that's easier than you chasing them", 76.0, false, false, ["INBOX", "L_INVESTORS"]],
  ["a3", "tomas@talleres.example", "Cotización — cocina Lomas", "Mandé la versión con el laminado alemán y la versión económica. La diferencia es 38 mil pesos, casi todo en", 82.0, false, true, ["INBOX"]],
  ["a1", "priya@northwind.example", "Design review notes — creator payouts", "The two-step confirmation is one step too many for repeat payouts. Suggest remembering the choice per creator", 90.0, false, false, ["INBOX"]],
  ["a4", "deb@feldmanlegal.example", "Property tax assessment appeal", "The county came back at 1.42M, which is above the comparables. Worth appealing — deadline is Sep 30", 98.0, false, true, ["INBOX", "L_FAMILY"]],
  ["a2", "marcus@lumen.example", "Q3 OKRs — draft 2", "Cut it from nine to four. The two that matter are activation rate and time-to-first-offer; the rest are inputs", 106.0, false, false, ["INBOX"]],
  ["a5", "ilya@meridian.example", "Airflow → Dagster migration, week 3", "Twelve of nineteen DAGs moved. The two that touch the Salesforce connector are going to be painful because of", 118.0, false, false, ["INBOX"]],
  ["a1", "billing-noreply@google.com", "Action required: OAuth verification", "Your project uses restricted scopes and requires verification. Submit your security assessment before Nov 1", 126.0, true, false, ["INBOX"]],
  ["a3", "rosa@talleres.example", "Cliente molesto — pedido #4390", "Llamó dos veces hoy. Le prometimos entrega el 28 y vamos a llegar el 4. Necesito autorización para el descuento", 140.0, false, false, ["INBOX", "L_CUSTOMERS"]],
  ["a1", "hana@shopify.com", "Feedback from the app review team", "Minor: your empty states assume the merchant has data. Reviewers install into a blank dev store, so screenshot", 154.0, false, false, ["INBOX"]],
  ["a4", "riley@example.com", "school picture day form", "its due friday. mom said to send it to you. also i need $22 for the package with the wallet size ones", 168.0, false, true, ["INBOX", "L_FAMILY"]],
  ["a2", "sam@stripe.com", "Dispute opened on charge ch_3PqL", "A cardholder disputed a $340.00 charge as 'product not received'. Respond with evidence by Aug 19", 182.0, false, false, ["INBOX"]],
  ["a1", "dana@northwind.example", "Regression suite is green again", "The three flaky tests were all the same root cause — a fixture that assumed the local timezone was UTC", 200.0, false, false, ["INBOX"]],
  ["a5", "aurora@linear.app", "Re: Meridian Data workspace", "Bumped you to 40 seats at the current rate through renewal. New seats after that price at the standard tier", 220.0, false, false, ["INBOX"]],
  ["a1", "cassidy@paperbark.example", "Notes from the Paperbark call", "Three asks: bulk offer import, a sandbox, and SSO. The first two are cheap, SSO is the real conversation", 244.0, false, true, ["INBOX", "L_CUSTOMERS"]],
  ["a2", "marcus@lumen.example", "Renewal terms — Paperbark", "They want annual with quarterly outs, which is not a thing. Countering with annual, 30-day termination for", 268.0, false, false, ["INBOX", "L_CUSTOMERS"]],
  ["a1", "tawny@northloop.example", "Term sheet — redline attached", "Two changes from what we discussed: the option pool is pre-money and the board seat converts at Series B", 292.0, false, true, ["INBOX", "L_INVESTORS"]],
  ["a4", "deb@feldmanlegal.example", "Re: Property tax assessment appeal", "Filed. Hearing is scheduled for Oct 14 at 9am, and you do not need to attend unless they contest the comps", 316.0, false, false, ["INBOX", "L_FAMILY"]],
];

const BODY_PARAGRAPHS = [
  "Following up on this so it doesn't get lost. I've pulled the numbers into the shared sheet and flagged the three rows that don't reconcile — they're all in the same date range, which makes me think it's an export boundary problem rather than anything real.",
  "The short version: we can do it, but not before the end of the month, and only if the other thing slips. I'd rather make that trade explicitly than discover it in two weeks.",
  "Let me know if you'd rather talk this through live. I have Thursday afternoon open and most of Friday.",
];

function seededBody(index: number, snippet: string): string {
  const paras = [snippet + "."];
  paras.push(BODY_PARAGRAPHS[index % BODY_PARAGRAPHS.length]);
  if (index % 3 === 0) paras.push(BODY_PARAGRAPHS[(index + 1) % BODY_PARAGRAPHS.length]);
  return paras.join("\n\n");
}

const ATTACHMENT_NAMES: [string, string, number][] = [
  ["cohort-retention-q3.xlsx", "application/vnd.ms-excel", 184_320],
  ["roadmap-q3-final.pdf", "application/pdf", 2_412_000],
  ["warehouse-costs-oct.csv", "text/csv", 41_200],
  ["qa-report.pdf", "application/pdf", 890_400],
  ["trust-restated-2026.pdf", "application/pdf", 3_918_000],
  ["invoice-INV-2026-08-0442.pdf", "application/pdf", 62_100],
  ["showroom-polanco.zip", "application/zip", 28_400_000],
  ["postmortem-checkout.pdf", "application/pdf", 512_000],
];

function build(): { threads: Thread[]; messages: Map<number, Message[]> } {
  const now = Date.now();
  const threads: Thread[] = [];
  const messages = new Map<number, Message[]>();

  THREAD_SEEDS.forEach((seed, i) => {
    const [accountKey, fromEmail, subject, snippet, hoursAgo, unread, hasAttachment, labelIds] = seed;
    const id = i + 1;
    const account = accountFor(accountKey);
    const accountId = account.id;
    const from = person(fromEmail);
    const me: Participant = { name: ME.name, email: account.email };
    const timestamp = now - hoursAgo * HOUR;
    const messageCount = (i % 4) + 1;

    threads.push({
      id,
      accountId,
      subject,
      snippet,
      participants: messageCount > 1 ? [from, me] : [from],
      timestamp,
      unread,
      starred: i % 9 === 0,
      hasAttachment,
      messageCount,
      labelIds,
    });

    const thread: Message[] = [];
    for (let m = 0; m < messageCount; m++) {
      const outbound = m % 2 === 1;
      const messageId = id * 100 + m + 1;
      const attachments: Attachment[] =
        hasAttachment && m === messageCount - 1
          ? [
              {
                id: messageId * 10,
                messageId,
                filename: ATTACHMENT_NAMES[i % ATTACHMENT_NAMES.length][0],
                mimeType: ATTACHMENT_NAMES[i % ATTACHMENT_NAMES.length][1],
                sizeBytes: ATTACHMENT_NAMES[i % ATTACHMENT_NAMES.length][2],
              },
            ]
          : [];
      thread.push({
        id: messageId,
        threadId: id,
        accountId,
        from: outbound ? me : from,
        to: outbound ? [from] : [me],
        cc: [],
        timestamp: timestamp - (messageCount - 1 - m) * 47 * MINUTE,
        bodyText: seededBody(i + m, m === messageCount - 1 ? snippet : subject),
        attachments,
      });
    }
    messages.set(id, thread);
  });

  return { threads, messages };
}

const built = build();
export const threads: Thread[] = built.threads;
export const messagesByThread: Map<number, Message[]> = built.messages;

/** [calendarId, title, dayOffsetFromMonday, startHour, durationHours, location?] */
type EventSeed = [string, string, number, number, number, string?];

const EVENT_SEEDS: EventSeed[] = [
  // Monday
  ["c1", "Standup", 0, 9, 0.25],
  ["c1", "Roadmap lock — Q3", 0, 9.5, 1],
  ["c2", "Checkout incident review", 0, 9.5, 0.75, "Zoom"],
  ["c5", "dbt naming decision", 0, 11, 0.5],
  ["c4", "Riley — orthodontist", 0, 15.5, 1, "Clinic"],
  ["c1", "1:1 Priya", 0, 13, 0.5],
  // Tuesday
  ["c1", "Standup", 1, 9, 0.25],
  ["c2", "Paperbark intro call", 1, 10, 0.75, "Google Meet"],
  ["c2", "Paperbark — RevOps deep dive", 1, 10.5, 1, "Google Meet"],
  ["c3", "Proveedor herrajes — llamada", 1, 12, 0.5],
  ["c1", "Design review: creator payouts", 1, 14, 1],
  ["c5", "Dagster migration sync", 1, 14.5, 0.5],
  ["c1", "Interview: staff eng (A)", 1, 16, 1],
  // Wednesday
  ["c1", "Standup", 2, 9, 0.25],
  ["c1", "Shopify partner review", 2, 9.5, 0.5],
  ["c4", "Lunch — Deb (trust docs)", 2, 12, 1, "Café Lume"],
  ["c1", "Interview: staff eng (B)", 2, 13, 1],
  ["c2", "OKR draft 2 walkthrough", 2, 13, 0.75],
  ["c5", "Warehouse cost review", 2, 13.5, 1],
  ["c3", "Revisión de nómina", 2, 16, 0.5],
  // Thursday
  ["c1", "Standup", 3, 9, 0.25],
  ["c1", "Northloop — data room walkthrough", 3, 14, 1.5, "Northloop, 3rd floor"],
  ["c1", "Prep: Northloop", 3, 12.5, 1],
  ["c4", "Basketball tryouts pickup", 3, 16, 1],
  ["c2", "Renewal terms — Paperbark", 3, 10, 0.5],
  // Friday
  ["c1", "Standup", 4, 9, 0.25],
  ["c1", "QA sign-off: reporting rewrite", 4, 10, 1],
  ["c5", "Meridian — weekly", 4, 11, 0.5],
  ["c3", "Cotización Lomas — cierre", 4, 12, 0.5],
  ["c1", "Demo day", 4, 15, 2, "All hands"],
  // Weekend
  ["c4", "Riley — game vs. Westview", 5, 10, 2, "Westview HS"],
  ["c4", "Dinner — the Navarros", 5, 18.5, 2.5],
  ["c4", "Long run", 6, 7.5, 1.5],
];

function buildEvents(): CalendarEvent[] {
  const monday = startOfWeek(Date.now());
  const out: CalendarEvent[] = [];
  let nextId = 1;

  // Three weeks of events so paging left and right is not an empty room.
  for (let week = -1; week <= 1; week++) {
    EVENT_SEEDS.forEach((seed, i) => {
      const [calendarId, title, dayOffset, startHour, duration, location] = seed;
      // Vary the shoulder weeks a little so they don't read as a photocopy.
      if (week !== 0 && (i + week) % 4 === 0) return;
      const day = startOfDay(addDays(monday, dayOffset + week * 7));
      const start = day.getTime() + startHour * HOUR;
      const calendar = calendars.find((c) => c.id === calendarId)!;
      const attendees = [people[(i + week + 15) % people.length], people[(i * 3 + 1) % people.length]];
      out.push({
        id: nextId++,
        calendarId,
        accountId: calendar.accountId,
        title,
        start,
        end: start + duration * HOUR,
        allDay: false,
        location,
        attendees,
        organizer: attendees[0],
        rsvp: i % 5 === 0 ? "needsAction" : "accepted",
        sourceThreadId: i % 7 === 0 ? threads[i % threads.length]?.id : undefined,
      });
    });

    // One all-day event per week.
    const allDayStart = startOfDay(addDays(monday, 2 + week * 7));
    out.push({
      id: nextId++,
      calendarId: "c4",
      accountId: 4,
      title: week === 0 ? "Riley — no school" : "Quarter close",
      start: allDayStart.getTime(),
      end: allDayStart.getTime() + DAY,
      allDay: true,
      attendees: [],
    });
  }

  return out;
}

export const events: CalendarEvent[] = buildEvents();
