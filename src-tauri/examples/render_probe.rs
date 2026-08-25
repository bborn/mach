//! What selecting a conversation actually costs, on a copy of the owner's store.
//!
//! The reading pane's critical path on `j`/`k` is sequential:
//!
//!   1. `get_thread` — every message in the conversation, bodies included
//!   2. JSON across the IPC boundary
//!   3. `render_message_body` — loads the whole thread *again* to find one row,
//!      then sanitizes the expanded message
//!   4. the WebView parses that HTML as an iframe `srcdoc`
//!
//! 1–3 are timed here. 4 is dumped as sanitized HTML for a Chrome harness.
//!
//! Never pointed at the live file. `Db::open` runs migrations.
//!
//! ```sh
//! sqlite3 "$HOME/Library/Application Support/com.mach.mail/mach.sqlite3" \
//!   ".backup /tmp/mach-render-probe/mach.sqlite3"
//! cargo run --release --example render_probe -- /tmp/mach-render-probe/mach.sqlite3
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use mach_lib::db::{queries, Db};
use mach_lib::ipc::reads;
use mach_lib::ipc::render::render_stored_message;
use mach_lib::ipc::types::{ThreadDetail, ThreadQuery};

const N: usize = 40;
const GMAIL: &str = "bruno.bornsztein@gmail.com";

struct Samples(Vec<f64>);

impl Samples {
    fn push(&mut self, ms: f64) {
        self.0.push(ms);
    }
    fn pct(&mut self, p: f64) -> f64 {
        if self.0.is_empty() {
            return 0.0;
        }
        self.0.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let i = ((self.0.len() - 1) as f64 * p).round() as usize;
        self.0[i]
    }
    fn report(&mut self, label: &str) {
        let n = self.0.len();
        let (p50, p95, max) = (self.pct(0.5), self.pct(0.95), self.pct(1.0));
        println!("  {label:<42} n={n:<4} p50 {p50:>7.2}ms  p95 {p95:>7.2}ms  max {max:>7.2}ms");
    }
}

fn ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

fn kb(bytes: usize) -> f64 {
    bytes as f64 / 1024.0
}

fn strip_bodies(mut detail: ThreadDetail) -> ThreadDetail {
    for message in &mut detail.messages {
        message.body_html = None;
        message.body_text = None;
    }
    detail
}

fn html_bytes(detail: &ThreadDetail) -> (usize, usize) {
    let total: usize = detail
        .messages
        .iter()
        .map(|m| m.body_html.as_deref().map(str::len).unwrap_or(0))
        .sum();
    let latest = detail
        .messages
        .last()
        .and_then(|m| m.body_html.as_deref())
        .map(str::len)
        .unwrap_or(0);
    (total, latest)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or("usage: render_probe <copy-of-mach.sqlite3>")?,
    );
    if !path.exists() {
        return Err(format!("{} does not exist", path.display()).into());
    }

    let dump = path
        .parent()
        .unwrap_or(Path::new("/tmp"))
        .join("bodies");
    fs::create_dir_all(&dump)?;

    println!("opening {}", path.display());
    let opened = Instant::now();
    let db = Db::open(&path)?;
    println!("  Db::open (includes migrate) {:>7.1}ms\n", ms(opened));

    let accounts = db.read(queries::list_accounts)?;
    for a in &accounts {
        println!("  account {:>2}  {}", a.id, a.email);
    }

    let gmail_id = accounts
        .iter()
        .find(|a| a.email == GMAIL)
        .map(|a| a.id)
        .ok_or("gmail.com account missing from this copy")?;

    let census = db.read(|conn| {
        let messages: i64 = conn.query_row("SELECT count(*) FROM messages", [], |r| r.get(0))?;
        let with_html: i64 = conn.query_row(
            "SELECT count(*) FROM messages WHERE body_html IS NOT NULL AND length(body_html) > 0",
            [],
            |r| r.get(0),
        )?;
        let html_bytes: i64 = conn.query_row(
            "SELECT coalesce(sum(length(body_html)), 0) FROM messages",
            [],
            |r| r.get(0),
        )?;
        Ok((messages, with_html, html_bytes))
    })?;
    println!(
        "\n  store: {} messages, {} with html, {:.0} MB of body_html\n",
        census.0,
        census.1,
        census.2 as f64 / (1024.0 * 1024.0)
    );

    let listed = Instant::now();
    let page = reads::list_threads(
        &db,
        &ThreadQuery {
            account_id: Some(gmail_id),
            label_id: Some("PRIMARY".into()),
            unread_only: false,
            limit: Some(N as u32),
            cursor: None,
        },
    )?;
    println!(
        "list_threads PRIMARY {} n={}  {:>6.2}ms\n",
        GMAIL,
        page.items.len(),
        ms(listed)
    );

    let mut get = Samples(Vec::new());
    let mut serde_full = Samples(Vec::new());
    let mut serde_lean = Samples(Vec::new());
    let mut load_all = Samples(Vec::new());
    let mut load_one = Samples(Vec::new());
    let mut sanitize_latest = Samples(Vec::new());
    let mut sanitize_all = Samples(Vec::new());
    let mut path_today = Samples(Vec::new());
    let mut path_if_lean = Samples(Vec::new());

    println!(
        "{:<6} {:>4} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}  subject",
        "id", "msgs", "html_kb", "lat_kb", "get", "json", "lean", "sani", "path"
    );

    let mut dumped = 0usize;
    let mut dumps: Vec<(String, String, usize)> = Vec::new();

    for (i, summary) in page.items.iter().enumerate() {
        let t0 = Instant::now();
        let detail = reads::get_thread(&db, summary.id)?;
        let get_ms = ms(t0);
        get.push(get_ms);

        let (html_total, html_latest) = html_bytes(&detail);
        let n_msg = detail.messages.len();

        let t1 = Instant::now();
        let json = serde_json::to_vec(&detail)?;
        let json_ms = ms(t1);
        serde_full.push(json_ms);

        let lean = strip_bodies(detail.clone());
        let t2 = Instant::now();
        let lean_json = serde_json::to_vec(&lean)?;
        let lean_ms = ms(t2);
        serde_lean.push(lean_ms);

        let t3 = Instant::now();
        let _again = db.read(|conn| queries::messages_for_thread(conn, summary.id))?;
        let load_all_ms = ms(t3);
        load_all.push(load_all_ms);

        let latest_id = detail.messages.last().map(|m| m.id);
        let load_one_ms = if let Some(id) = latest_id {
            let t = Instant::now();
            let _: Option<String> = db.read(|conn| {
                Ok(conn
                    .query_row(
                        "SELECT body_html FROM messages WHERE id = ?1",
                        [id],
                        |row| row.get(0),
                    )
                    .ok())
            })?;
            let one = ms(t);
            load_one.push(one);
            one
        } else {
            0.0
        };

        let t4 = Instant::now();
        let rendered = detail
            .messages
            .last()
            .map(|m| render_stored_message(m, true));
        let sani_ms = ms(t4);
        sanitize_latest.push(sani_ms);

        let t5 = Instant::now();
        for m in &detail.messages {
            let _ = render_stored_message(m, true);
        }
        sanitize_all.push(ms(t5));

        // Today's critical path, CPU only: get_thread + JSON + reload thread + sanitize latest.
        let today = get_ms + json_ms + load_all_ms + sani_ms;
        path_today.push(today);
        // Lean path: get_thread still does the SQL (bodies in the row mapping),
        // but the wire is headers-only and the second load is one column.
        let lean_path = get_ms + lean_ms + load_one_ms + sani_ms;
        path_if_lean.push(lean_path);

        let subject: String = summary
            .subject
            .chars()
            .take(48)
            .collect();
        println!(
            "{:<6} {:>4} {:>8.1} {:>8.1} {:>8.2} {:>8.2} {:>8.2} {:>8.2} {:>8.2}  {subject}",
            summary.id,
            n_msg,
            kb(html_total),
            kb(html_latest),
            get_ms,
            json_ms,
            lean_ms,
            sani_ms,
            today
        );

        if let Some(body) = rendered {
            if !body.body.html.is_empty() && dumped < 12 {
                let name = format!(
                    "{:02}-{}msg-{}kb.html",
                    i,
                    n_msg,
                    (html_latest / 1024).max(1)
                );
                fs::write(dump.join(&name), &body.body.html)?;
                dumps.push((name, body.body.html, html_latest));
                dumped += 1;
            }
        }

        let _ = (json.len(), lean_json.len());
        if i == 0 {
            println!(
                "       first thread json {} KB  lean {} KB  ({:.0}% of the payload is bodies)",
                json.len() / 1024,
                lean_json.len() / 1024,
                100.0 * (1.0 - lean_json.len() as f64 / json.len().max(1) as f64)
            );
        }
    }

    // Threads the first PRIMARY page does not represent: the heaviest inbox
    // conversations, plus the 6.5 MB Honeybadger thread sitting in All Mail.
    println!("\n--- heavy threads (inbox + the worst archived) ---");
    println!(
        "{:<6} {:>4} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}  subject",
        "id", "msgs", "html_kb", "json_kb", "get", "reload", "sani", "path"
    );
    for id in [28485i64, 28490, 784, 48080, 28688] {
        let t0 = Instant::now();
        let detail = match reads::get_thread(&db, id) {
            Ok(d) => d,
            Err(e) => {
                println!("{id:<6}  skip: {e}");
                continue;
            }
        };
        let get_ms = ms(t0);
        let (html_total, html_latest) = html_bytes(&detail);
        let n_msg = detail.messages.len();
        let t1 = Instant::now();
        let json = serde_json::to_vec(&detail)?;
        let json_ms = ms(t1);
        let t2 = Instant::now();
        let _ = db.read(|conn| queries::messages_for_thread(conn, id))?;
        let reload = ms(t2);
        let t3 = Instant::now();
        let rendered = detail
            .messages
            .last()
            .map(|m| render_stored_message(m, true));
        let sani = ms(t3);
        let subject: String = detail.thread.subject.chars().take(44).collect();
        println!(
            "{id:<6} {n_msg:>4} {:>8.1} {:>8} {get_ms:>8.2} {reload:>8.2} {sani:>8.2} {:>8.2}  {subject}",
            kb(html_total),
            json.len() / 1024,
            get_ms + json_ms + reload + sani
        );
        if let Some(body) = rendered {
            if !body.body.html.is_empty() {
                let name = format!(
                    "heavy-{}-{}msg-{}kb.html",
                    id,
                    n_msg,
                    (html_latest / 1024).max(1)
                );
                fs::write(dump.join(&name), &body.body.html)?;
                dumps.push((name, body.body.html, html_latest));
            }
        }
    }

    println!("\n--- distributions, first {N} PRIMARY threads ---");
    get.report("get_thread (sql + unsub)");
    serde_full.report("serde JSON (bodies included)");
    serde_lean.report("serde JSON (headers only)");
    load_all.report("messages_for_thread again");
    load_one.report("SELECT body_html WHERE id=?");
    sanitize_latest.report("sanitize latest message");
    sanitize_all.report("sanitize every message");
    path_today.report("today: get+json+reload+sani");
    path_if_lean.report("lean:  get+lean json+1 col+sani");

    // A walk: twenty sequential opens, as holding `j` does.
    let walk_n = page.items.len().min(20);
    let t = Instant::now();
    for summary in page.items.iter().take(walk_n) {
        let detail = reads::get_thread(&db, summary.id)?;
        let _ = serde_json::to_vec(&detail)?;
        let _ = db.read(|conn| queries::messages_for_thread(conn, summary.id))?;
        if let Some(m) = detail.messages.last() {
            let _ = render_stored_message(m, true);
        }
    }
    println!(
        "\nwalk {walk_n} sequential opens (today's CPU path): {:>6.1}ms  ({:.1}ms each)",
        ms(t),
        ms(t) / walk_n as f64
    );

    write_iframe_harness(&dump, &dumps)?;
    println!("\ndumped {} sanitized bodies in {}", dumps.len(), dump.display());
    Ok(())
}

fn write_iframe_harness(
    dir: &Path,
    dumps: &[(String, String, usize)],
) -> Result<(), Box<dyn std::error::Error>> {
    let payloads: Vec<serde_json::Value> = dumps
        .iter()
        .map(|(name, html, raw_kb)| {
            serde_json::json!({
                "name": name,
                "rawKb": (*raw_kb as f64 / 1024.0 * 10.0).round() / 10.0,
                "html": html,
            })
        })
        .collect();
    let json = serde_json::to_string(&payloads)?;
    let page = format!(
        r#"<!doctype html>
<meta charset="utf-8">
<title>render-probe</title>
<pre id="out">running</pre>
<script>
const bodies = {json};
const out = document.getElementById("out");
function percentile(xs, p) {{
  const s = [...xs].sort((a,b) => a-b);
  return s[Math.round((s.length-1)*p)];
}}
async function once(html) {{
  return new Promise((resolve) => {{
    const iframe = document.createElement("iframe");
    iframe.sandbox = "allow-same-origin";
    iframe.style.cssText = "position:absolute;left:-10000px;width:640px;height:10px;border:0";
    const t0 = performance.now();
    iframe.onload = () => {{
      const doc = iframe.contentDocument;
      const h = Math.max(
        doc.documentElement.scrollHeight,
        doc.body ? doc.body.scrollHeight : 0,
      );
      const ms = performance.now() - t0;
      iframe.remove();
      resolve({{ms, h}});
    }};
    iframe.srcdoc = html;
    document.body.appendChild(iframe);
  }});
}}
(async () => {{
  const rows = [];
  for (const body of bodies) {{
    const times = [];
    let h = 0;
    for (let i = 0; i < 6; i++) {{
      const r = await once(body.html);
      if (i > 0) times.push(r.ms);
      h = r.h;
    }}
    rows.push({{
      name: body.name,
      rawKb: body.rawKb,
      srcdocKb: Math.round(body.html.length/102.4)/10,
      height: h,
      p50: Math.round(percentile(times, 0.5)*10)/10,
      p95: Math.round(percentile(times, 0.95)*10)/10,
      max: Math.round(percentile(times, 1)*10)/10,
    }});
  }}
  out.textContent = JSON.stringify({{engine: navigator.userAgent, rows}}, null, 2);
  document.title = "render-probe-done";
}})().catch((e) => {{ out.textContent = String(e); }});
</script>
"#
    );
    fs::write(dir.join("iframe.html"), page)?;
    Ok(())
}
