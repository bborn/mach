//! The editor's grammar: markdown-ish source → clean HTML.
//!
//! Restraint is the feature. This is the part of Spark that got called stupid,
//! and the reason is that its composer is a rich-text editor pretending to be a
//! document: a toolbar, a paste path that carries somebody else's fonts, and a
//! DOM you fight instead of type into. So the editor here is a `<textarea>`,
//! the source of truth is the characters the user typed, and the HTML is
//! produced once — at send — by this function.
//!
//! The grammar is deliberately small. Everything it does not recognise stays
//! literal, which is the property that makes it safe to type in: no rule can
//! surprise you by firing.
//!
//! | source | html |
//! |---|---|
//! | blank-line-separated blocks | `<p>` |
//! | single newline inside a block | `<br>` |
//! | `**bold**` | `<strong>` |
//! | `*italic*`, `_italic_` | `<em>` |
//! | `` `code` `` | `<code>` |
//! | `- ` / `* ` / `+ ` | `<ul><li>` |
//! | `1. ` | `<ol><li>` |
//! | `> ` | `<blockquote>` |
//! | `# ` … `### ` | `<h1>` … `<h3>` |
//! | ```` ``` ```` fence | `<pre><code>` |
//! | a bare `https://…` | `<a href>` |
//!
//! There is no image syntax, no table syntax, and no raw HTML passthrough:
//! every `<` in the source is escaped before anything else happens, so the
//! output cannot contain markup the user did not ask for.
//!
//! `src/lib/compose.ts` mirrors this for the optimistic local copy the thread
//! shows the instant you hit send. The two are pinned to the same table of
//! cases in `tests/compose.rs` and `src/lib/compose.test.ts`; if they drift,
//! both suites fail, which is the only way two implementations of one grammar
//! can be allowed to exist.

/// Render markdown-ish source to a self-contained HTML fragment.
pub fn to_html(source: &str) -> String {
    let normalized = source.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();
    let mut out = String::with_capacity(source.len() * 2);
    let mut i = 0usize;

    while i < lines.len() {
        let line = lines[i];

        if line.trim().is_empty() {
            i += 1;
            continue;
        }

        // ``` fenced code — taken verbatim, no inline rules inside.
        if line.trim_start().starts_with("```") {
            let mut body = String::new();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                if !body.is_empty() {
                    body.push('\n');
                }
                body.push_str(lines[i]);
                i += 1;
            }
            i += 1; // the closing fence, or the end of input
            out.push_str("<pre><code>");
            escape_into(&mut out, &body);
            out.push_str("</code></pre>");
            continue;
        }

        if let Some((level, text)) = heading(line) {
            out.push_str(&format!("<h{level}>"));
            out.push_str(&inline(text));
            out.push_str(&format!("</h{level}>"));
            i += 1;
            continue;
        }

        if quote_body(line).is_some() {
            let mut body: Vec<&str> = Vec::new();
            while i < lines.len() {
                match quote_body(lines[i]) {
                    Some(text) => {
                        body.push(text);
                        i += 1;
                    }
                    None => break,
                }
            }
            out.push_str("<blockquote>");
            out.push_str(&paragraph(&body.join("\n")));
            out.push_str("</blockquote>");
            continue;
        }

        if bullet_body(line).is_some() {
            out.push_str("<ul>");
            while i < lines.len() {
                match bullet_body(lines[i]) {
                    Some(text) => {
                        out.push_str("<li>");
                        out.push_str(&inline(text));
                        out.push_str("</li>");
                        i += 1;
                    }
                    None => break,
                }
            }
            out.push_str("</ul>");
            continue;
        }

        if ordered_body(line).is_some() {
            out.push_str("<ol>");
            while i < lines.len() {
                match ordered_body(lines[i]) {
                    Some(text) => {
                        out.push_str("<li>");
                        out.push_str(&inline(text));
                        out.push_str("</li>");
                        i += 1;
                    }
                    None => break,
                }
            }
            out.push_str("</ol>");
            continue;
        }

        // A plain paragraph: everything up to the next blank line or block start.
        let mut body: Vec<&str> = Vec::new();
        while i < lines.len() && !lines[i].trim().is_empty() && !starts_block(lines[i]) {
            body.push(lines[i]);
            i += 1;
        }
        out.push_str(&paragraph(&body.join("\n")));
    }

    if out.is_empty() {
        "<p></p>".to_string()
    } else {
        out
    }
}

/// The plain-text part. The markdown source *is* the plain-text part — that is
/// the point of a markdown-ish editor, and it is why the `text/plain` half of
/// the message reads as something a person wrote rather than as HTML with the
/// tags pulled out.
pub fn to_text(source: &str) -> String {
    source.replace("\r\n", "\n").replace('\r', "\n")
}

fn starts_block(line: &str) -> bool {
    heading(line).is_some()
        || quote_body(line).is_some()
        || bullet_body(line).is_some()
        || ordered_body(line).is_some()
        || line.trim_start().starts_with("```")
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 3 {
        return None;
    }
    let rest = &line[hashes..];
    let text = rest.strip_prefix(' ')?;
    Some((hashes, text.trim()))
}

fn quote_body(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('>')?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

fn bullet_body(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            return Some(rest.trim());
        }
    }
    None
}

fn ordered_body(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let digits = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let rest = &trimmed[digits..];
    let rest = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')'))?;
    Some(rest.strip_prefix(' ')?.trim())
}

fn paragraph(body: &str) -> String {
    format!("<p>{}</p>", inline(body).replace('\n', "<br>"))
}

/// Inline rules.
///
/// Two things are lifted out before the emphasis pass and put back after it,
/// because both contain characters the emphasis rules would otherwise eat:
/// code spans (a backtick span must stay literal all the way down, so
/// `` `**not bold**` `` renders as `**not bold**`) and URLs (`_` is legal and
/// common in a path, and turning `/a_b_c` into `/a<em>b</em>c` inside an `href`
/// is a broken link, not a typo).
fn inline(text: &str) -> String {
    let mut fragments: Vec<String> = Vec::new();
    let without_code = mask_code_spans(text, &mut fragments);
    let escaped = escape(&without_code);
    let without_links = mask_links(&escaped, &mut fragments);
    let emphasised = emphasis(&without_links);
    unmask(&emphasised, &fragments)
}

/// U+0000 cannot reach here — the editor strips it and SQLite's TEXT handling
/// would truncate on it — so it is a placeholder alphabet nothing can collide
/// with.
const MASK: char = '\u{0}';

fn placeholder(index: usize) -> String {
    format!("{MASK}{index}{MASK}")
}

fn mask_code_spans(text: &str, fragments: &mut Vec<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('`') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('`') else { break };
        out.push_str(&rest[..open]);
        out.push_str(&placeholder(fragments.len()));
        fragments.push(format!("<code>{}</code>", escape(&after[..close])));
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Bare `http(s)://…` becomes a link. Nothing else does — a rule that fires on
/// `example.com` turns every version number into an anchor.
fn mask_links(escaped: &str, fragments: &mut Vec<String>) -> String {
    let mut out = String::with_capacity(escaped.len());
    let mut rest = escaped;
    while let Some(at) = find_scheme(rest) {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let end = tail
            .find(|c: char| c.is_whitespace() || c == '<' || c == MASK)
            .unwrap_or(tail.len());
        // Trailing punctuation belongs to the sentence, not to the URL.
        let mut url = &tail[..end];
        while let Some(last) = url.chars().last() {
            if matches!(last, '.' | ',' | ')' | ']' | '!' | '?' | ':' | ';') {
                url = &url[..url.len() - last.len_utf8()];
            } else {
                break;
            }
        }
        if url.is_empty() {
            out.push_str(tail);
            return out;
        }
        out.push_str(&placeholder(fragments.len()));
        fragments.push(format!("<a href=\"{url}\">{url}</a>"));
        rest = &tail[url.len()..];
    }
    out.push_str(rest);
    out
}

fn find_scheme(text: &str) -> Option<usize> {
    let mut from = 0usize;
    while let Some(found) = text[from..].find("http") {
        let at = from + found;
        let tail = &text[at..];
        if tail.starts_with("https://") || tail.starts_with("http://") {
            return Some(at);
        }
        from = at + 4;
    }
    None
}

fn unmask(text: &str, fragments: &[String]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find(MASK) {
        let after = &rest[open + MASK.len_utf8()..];
        let Some(close) = after.find(MASK) else { break };
        let Ok(index) = after[..close].parse::<usize>() else { break };
        out.push_str(&rest[..open]);
        out.push_str(fragments.get(index).map(String::as_str).unwrap_or(""));
        rest = &after[close + MASK.len_utf8()..];
    }
    out.push_str(rest);
    out
}

fn emphasis(text: &str) -> String {
    let bold = wrap(text, "**", "<strong>", "</strong>");
    let italic = wrap(&bold, "*", "<em>", "</em>");
    wrap(&italic, "_", "<em>", "</em>")
}

/// Replace balanced `marker…marker` pairs. An unmatched marker is left alone —
/// a lone asterisk in a sentence is punctuation, not a broken tag.
fn wrap(text: &str, marker: &str, open: &str, close: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let Some(start) = rest.find(marker) else { break };
        let after = &rest[start + marker.len()..];
        let Some(end) = after.find(marker) else { break };
        let inner = &after[..end];
        // `** **` and `a * b * c` are not emphasis; requiring non-blank content
        // that neither starts nor ends with a space is what keeps prose alone.
        if inner.trim().is_empty() || inner.starts_with(' ') || inner.ends_with(' ') {
            out.push_str(&rest[..start + marker.len()]);
            rest = after;
            continue;
        }
        out.push_str(&rest[..start]);
        out.push_str(open);
        out.push_str(inner);
        out.push_str(close);
        rest = &after[end + marker.len()..];
    }
    out.push_str(rest);
    out
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    escape_into(&mut out, s);
    out
}

pub fn escape_into(out: &mut String, s: &str) {
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
}
