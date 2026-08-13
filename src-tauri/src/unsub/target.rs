//! Reading `List-Unsubscribe` into something it is safe to act on.
//!
//! Everything in this file treats the header as hostile input, because it is:
//! it is a string a stranger put in a message and the whole point of the
//! feature is that the app will make a network request or send a mail because
//! of it. Nothing here performs either — this is parsing and validation only,
//! and the types it produces are the only way to describe an unsubscribe
//! elsewhere in the app.
//!
//! # The shape of the header
//!
//! RFC 2369 says a comma-separated list of URLs, each in angle brackets:
//!
//! ```text
//! List-Unsubscribe: <https://example.com/u/abc>, <mailto:u@example.com?subject=unsub>
//! ```
//!
//! Splitting on commas is wrong — a URL may contain one, and several real
//! senders put one in a query string. So the scan is for bracket *pairs*, and
//! a comma inside a pair is part of the URL. A header with no brackets at all
//! is common enough in the wild to be worth tolerating, and is read as one URL.
//!
//! # Which schemes are allowed, and why only two
//!
//! `https` and `mailto`. Nothing else, ever.
//!
//! * `http` is refused rather than upgraded. An unsubscribe is an assertion
//!   that this address is live and read, and sending it in clear text over a
//!   network hands that assertion to anyone on the path. Upgrading to `https`
//!   silently would also mean requesting a URL the sender never published.
//! * `javascript:`, `data:` and `file:` are refused because they are only
//!   dangerous if something navigates to them, and the way to be sure nothing
//!   ever does is never to hold one in the first place.
//! * everything else is refused because an allowlist that grows by exception
//!   is not an allowlist.
//!
//! # What else an `https` target has to survive
//!
//! | check | what it stops |
//! |---|---|
//! | no userinfo (`https://user:pw@host/`) | credentials aimed at whatever the request reaches |
//! | a host that is present and is not `localhost` | a POST at this machine |
//! | not a loopback, private, link-local or unique-local IP literal | a POST at the LAN, or at Mach's own QA port |
//! | at most [`MAX_URL_LEN`] bytes | a header written to be a denial of service |
//!
//! The IP checks are on *literals in the URL*. A hostname that resolves to a
//! private address defeats them; see the module docs on [`super`] for why that
//! residual risk is accepted and what would close it.
//!
//! # `mailto`, and the parts of it that are ignored
//!
//! RFC 6068 lets a `mailto:` carry arbitrary headers in its query string,
//! including `to`, `cc` and `bcc`. Mach honours **`subject` and `body` and
//! nothing else**: a sender who could add recipients could use the owner's own
//! account to mail a third party, which is a far more interesting thing to do
//! with this feature than unsubscribing anybody.

use std::net::IpAddr;

use url::{Host, Url};

/// The longest `List-Unsubscribe` URL that will be looked at. Real ones are
/// under 200 bytes; this is only here so a pathological header cannot become a
/// pathological request.
pub const MAX_URL_LEN: usize = 2048;

/// How many addresses one `mailto:` may name. More than one is unusual and
/// more than a few is not an unsubscribe.
pub const MAX_MAILTO_RECIPIENTS: usize = 4;

/// The subject used when a `mailto:` names none. Every list processor that
/// cares specifies one; this is for the ones that do not.
pub const DEFAULT_MAILTO_SUBJECT: &str = "unsubscribe";

/// The exact body RFC 8058 requires, and the exact `Content-Type` it goes with.
pub const ONE_CLICK_BODY: &str = "List-Unsubscribe=One-Click";
pub const ONE_CLICK_CONTENT_TYPE: &str = "application/x-www-form-urlencoded";

/// The `List-Unsubscribe-Post` value that turns an `https` target into a
/// one-click one. Compared case-insensitively after trimming, because the
/// header is written by hand by a great many people.
const ONE_CLICK_POST_HEADER: &str = "list-unsubscribe=one-click";

/// One way to unsubscribe, already validated.
///
/// Holding one of these is a claim that the scheme is allowed, the host is
/// reachable-and-not-us, and the strings are the ones to use verbatim. Nothing
/// downstream re-checks, which is why nothing may construct one except
/// [`parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// RFC 8058: one `POST` of [`ONE_CLICK_BODY`] and it is done, with no page
    /// and no browser.
    OneClick { url: String },
    /// RFC 2369 `mailto:`. Mach already sends mail; this is a send with no
    /// composer.
    Mail {
        to: Vec<String>,
        subject: String,
        body: Option<String>,
    },
    /// An `https` URL with no one-click support. It might unsubscribe on `GET`
    /// and it might be a form. Mach does not guess — see [`super`].
    Link { url: String },
}

impl Target {
    /// A stable name for a log line, a test, and the agent catalogue.
    pub fn kind(&self) -> &'static str {
        match self {
            Target::OneClick { .. } => "oneClick",
            Target::Mail { .. } => "mail",
            Target::Link { .. } => "link",
        }
    }

    /// Whether Mach can carry this out on its own, with no browser and no
    /// human. False for [`Target::Link`], which is the whole point of it.
    pub fn is_automatic(&self) -> bool {
        !matches!(self, Target::Link { .. })
    }
}

/// Why a header yielded nothing to act on. Not an error type — a header with
/// no usable target is an ordinary fact about a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unusable {
    /// Absent, or nothing but whitespace.
    Empty,
    /// Brackets that never closed, or bracket pairs with nothing inside.
    Malformed,
    /// Every candidate used a scheme that is not `https` or `mailto`.
    BadScheme,
    /// An `https` URL that named this machine, a private network, or nobody.
    UnsafeHost,
    /// A `mailto:` with no address in it.
    NoRecipient,
    /// Longer than [`MAX_URL_LEN`].
    TooLong,
}

impl Unusable {
    pub fn as_str(self) -> &'static str {
        match self {
            Unusable::Empty => "empty",
            Unusable::Malformed => "malformed",
            Unusable::BadScheme => "badScheme",
            Unusable::UnsafeHost => "unsafeHost",
            Unusable::NoRecipient => "noRecipient",
            Unusable::TooLong => "tooLong",
        }
    }
}

/// Turn `List-Unsubscribe` (and `List-Unsubscribe-Post`) into the one target
/// Mach will use.
///
/// Preference order, and the reason for it: one-click needs no page and no
/// human, `mailto:` needs no page either but takes a round trip through the
/// outbox, and a bare link needs a browser and a person. Best available wins.
///
/// `Err` names the *most specific* thing that went wrong across all the
/// candidates, so "there was an `https` URL but it pointed at 127.0.0.1" does
/// not read as "bad scheme".
pub fn parse(
    list_unsubscribe: Option<&str>,
    list_unsubscribe_post: Option<&str>,
) -> Result<Target, Unusable> {
    let header = list_unsubscribe.unwrap_or("").trim();
    if header.is_empty() {
        return Err(Unusable::Empty);
    }

    let candidates = split_candidates(header);
    if candidates.is_empty() {
        return Err(Unusable::Malformed);
    }

    let one_click = is_one_click(list_unsubscribe_post);

    let mut https: Option<String> = None;
    let mut mail: Option<Target> = None;
    // The first refusal that is more interesting than "we did not recognise
    // the scheme". A header of `<javascript:…>` should say bad scheme; a
    // header of `<https://127.0.0.1/u>` should say unsafe host.
    let mut refusal: Option<Unusable> = None;
    let mut note = |reason: Unusable| {
        if refusal.is_none() || refusal == Some(Unusable::BadScheme) {
            refusal = Some(reason);
        }
    };

    for candidate in candidates {
        if candidate.len() > MAX_URL_LEN {
            note(Unusable::TooLong);
            continue;
        }
        match scheme_of(candidate) {
            Some(scheme) if scheme == "https" => match parse_https(candidate) {
                Ok(url) => {
                    if https.is_none() {
                        https = Some(url);
                    }
                }
                Err(reason) => note(reason),
            },
            Some(scheme) if scheme == "mailto" => match parse_mailto(candidate) {
                Ok(target) => {
                    if mail.is_none() {
                        mail = Some(target);
                    }
                }
                Err(reason) => note(reason),
            },
            _ => note(Unusable::BadScheme),
        }
    }

    if let Some(url) = https {
        if one_click {
            return Ok(Target::OneClick { url });
        }
        // A sender who offered both, without one-click, gets the mail: it is
        // the one Mach can finish by itself.
        if let Some(target) = mail {
            return Ok(target);
        }
        return Ok(Target::Link { url });
    }
    if let Some(target) = mail {
        return Ok(target);
    }
    Err(refusal.unwrap_or(Unusable::Malformed))
}

/// Would this URL have been accepted as an `https` target?
///
/// The redirect policy in [`super::http`] asks this about every hop, so a
/// sender cannot use a `302` to reach somewhere the header itself was refused.
/// It is the same function, which is the point — a second copy of the host
/// rules would be a second thing to keep right.
pub fn accepts_url(url: &str) -> bool {
    url.len() <= MAX_URL_LEN
        && scheme_of(url).as_deref() == Some("https")
        && parse_https(url).is_ok()
}

/// Whether `List-Unsubscribe-Post` says one-click.
pub fn is_one_click(header: Option<&str>) -> bool {
    header
        .map(|v| v.trim().eq_ignore_ascii_case(ONE_CLICK_POST_HEADER))
        .unwrap_or(false)
}

/// Pull the `<…>` pairs out of a header value.
///
/// Anything outside a pair is dropped, which is what makes RFC 5322 comments
/// and stray commas harmless. A value with no `<` at all is returned whole,
/// because senders who write bare URLs are common and refusing them would cost
/// real unsubscribes to buy nothing.
fn split_candidates(header: &str) -> Vec<&str> {
    if !header.contains('<') {
        let single = header.trim();
        return if single.is_empty() { Vec::new() } else { vec![single] };
    }

    let mut out = Vec::new();
    let bytes = header.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let start = i + 1;
        match header[start..].find('>') {
            Some(offset) => {
                let inner = header[start..start + offset].trim();
                if !inner.is_empty() {
                    out.push(inner);
                }
                i = start + offset + 1;
            }
            // An unclosed bracket. Everything after it is not a URL anybody
            // wrote on purpose, so stop rather than guess where it ended.
            None => break,
        }
    }
    out
}

/// The lowercased scheme, if the string has one at all.
fn scheme_of(candidate: &str) -> Option<String> {
    let colon = candidate.find(':')?;
    let scheme = &candidate[..colon];
    if scheme.is_empty() {
        return None;
    }
    // A scheme is ALPHA *( ALPHA / DIGIT / "+" / "-" / "." ). Anything else and
    // the colon belonged to something other than a scheme.
    let mut chars = scheme.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.') {
        return None;
    }
    Some(scheme.to_ascii_lowercase())
}

fn parse_https(candidate: &str) -> Result<String, Unusable> {
    let url = Url::parse(candidate).map_err(|_| Unusable::Malformed)?;
    // `Url::parse` normalises the scheme, and a scheme-relative oddity could
    // have slipped past the textual check above.
    if url.scheme() != "https" {
        return Err(Unusable::BadScheme);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Unusable::UnsafeHost);
    }
    match url.host() {
        Some(Host::Domain(name)) => {
            let name = name.trim_end_matches('.').to_ascii_lowercase();
            if name.is_empty() || name == "localhost" || name.ends_with(".localhost") {
                return Err(Unusable::UnsafeHost);
            }
        }
        Some(Host::Ipv4(addr)) => {
            if !routable(IpAddr::V4(addr)) {
                return Err(Unusable::UnsafeHost);
            }
        }
        Some(Host::Ipv6(addr)) => {
            if !routable(IpAddr::V6(addr)) {
                return Err(Unusable::UnsafeHost);
            }
        }
        None => return Err(Unusable::UnsafeHost),
    }
    let out = url.to_string();
    if out.len() > MAX_URL_LEN {
        return Err(Unusable::TooLong);
    }
    Ok(out)
}

/// Whether an IP literal is somewhere a stranger's unsubscribe is allowed to
/// point. Loopback is the one that matters most: Mach itself opens a loopback
/// port in development builds.
fn routable(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => {
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                // 100.64.0.0/10, carrier-grade NAT — also where Tailscale lives.
                || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1])))
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() || v6.is_multicast() {
                return false;
            }
            let seg = v6.segments()[0];
            // fc00::/7 unique-local, fe80::/10 link-local.
            if (seg & 0xfe00) == 0xfc00 || (seg & 0xffc0) == 0xfe80 {
                return false;
            }
            // An IPv4-mapped address is an IPv4 address wearing a hat.
            match v6.to_ipv4_mapped() {
                Some(v4) => routable(IpAddr::V4(v4)),
                None => true,
            }
        }
    }
}

fn parse_mailto(candidate: &str) -> Result<Target, Unusable> {
    let rest = candidate
        .get("mailto:".len()..)
        .ok_or(Unusable::NoRecipient)?;
    let (addresses, query) = match rest.split_once('?') {
        Some((a, q)) => (a, Some(q)),
        None => (rest, None),
    };

    let mut to: Vec<String> = Vec::new();
    for raw in addresses.split(',') {
        let decoded = percent_decode(raw.trim());
        let address = decoded.trim();
        if address.is_empty() {
            continue;
        }
        if !looks_like_address(address) {
            continue;
        }
        if !to.iter().any(|a| a.eq_ignore_ascii_case(address)) {
            to.push(address.to_string());
        }
        if to.len() == MAX_MAILTO_RECIPIENTS {
            break;
        }
    }
    if to.is_empty() {
        return Err(Unusable::NoRecipient);
    }

    let mut subject: Option<String> = None;
    let mut body: Option<String> = None;
    if let Some(query) = query {
        for pair in query.split('&') {
            let (key, value) = match pair.split_once('=') {
                Some(kv) => kv,
                None => continue,
            };
            // `to`, `cc`, `bcc` and every other header RFC 6068 permits are
            // dropped on the floor. See the module doc.
            match key.trim().to_ascii_lowercase().as_str() {
                "subject" if subject.is_none() => subject = Some(percent_decode(value)),
                "body" if body.is_none() => body = Some(percent_decode(value)),
                _ => {}
            }
        }
    }

    let subject = subject
        .map(|s| sanitise_header_value(&s))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_MAILTO_SUBJECT.to_string());

    Ok(Target::Mail {
        to,
        subject,
        body: body.filter(|b| !b.is_empty()),
    })
}

/// A subject arrives from a stranger and goes into a header. CR and LF are the
/// two bytes that would turn it into two headers.
fn sanitise_header_value(value: &str) -> String {
    value
        .chars()
        .map(|c| if c == '\r' || c == '\n' { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

/// Deliberately not a full address parser. It is a check that the string is
/// one addr-spec and not a display name, a header injection, or a list.
fn looks_like_address(address: &str) -> bool {
    if address.len() > 320 {
        return false;
    }
    if address
        .chars()
        .any(|c| c.is_whitespace() || c == '<' || c == '>' || c == ',' || c == ';' || c == '"')
    {
        return false;
    }
    let (local, domain) = match address.split_once('@') {
        Some(split) => split,
        None => return false,
    };
    if local.is_empty() || domain.is_empty() || domain.contains('@') {
        return false;
    }
    domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

/// RFC 6068 percent-decoding. `+` is *not* a space here: a `mailto:` query is
/// not a form body, and a subject of `a+b` means `a+b`.
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_click_header() -> Option<&'static str> {
        Some("List-Unsubscribe=One-Click")
    }

    // ---------------------------------------------------------------- shape

    #[test]
    fn an_absent_or_blank_header_has_nothing_in_it() {
        assert_eq!(parse(None, None), Err(Unusable::Empty));
        assert_eq!(parse(Some("   "), None), Err(Unusable::Empty));
    }

    #[test]
    fn a_bare_url_with_no_brackets_is_read_whole() {
        assert_eq!(
            parse(Some("https://example.com/u/abc"), None),
            Ok(Target::Link {
                url: "https://example.com/u/abc".into()
            })
        );
    }

    #[test]
    fn a_comma_inside_a_url_does_not_split_it() {
        let header = "<https://example.com/u?ids=1,2,3>";
        assert_eq!(
            parse(Some(header), None),
            Ok(Target::Link {
                url: "https://example.com/u?ids=1,2,3".into()
            })
        );
    }

    #[test]
    fn an_unclosed_bracket_is_malformed() {
        assert_eq!(parse(Some("<https://example.com/u"), None), Err(Unusable::Malformed));
    }

    // ----------------------------------------------------------- one-click

    #[test]
    fn one_click_needs_both_the_post_header_and_an_https_url() {
        let header = "<https://example.com/u/abc>";
        assert_eq!(
            parse(Some(header), one_click_header()),
            Ok(Target::OneClick {
                url: "https://example.com/u/abc".into()
            })
        );
        assert_eq!(
            parse(Some(header), None),
            Ok(Target::Link {
                url: "https://example.com/u/abc".into()
            })
        );
    }

    #[test]
    fn the_post_header_is_matched_loosely_on_case_and_space() {
        for value in [
            "List-Unsubscribe=One-Click",
            "list-unsubscribe=one-click",
            "  LIST-UNSUBSCRIBE=ONE-CLICK  ",
        ] {
            assert!(is_one_click(Some(value)), "{value} should be one-click");
        }
        for value in ["List-Unsubscribe=One Click", "One-Click", ""] {
            assert!(!is_one_click(Some(value)), "{value} should not be one-click");
        }
    }

    #[test]
    fn one_click_wins_over_a_mailto_in_the_same_header() {
        let header = "<mailto:u@example.com?subject=unsub>, <https://example.com/u/abc>";
        assert_eq!(
            parse(Some(header), one_click_header()),
            Ok(Target::OneClick {
                url: "https://example.com/u/abc".into()
            })
        );
    }

    #[test]
    fn without_one_click_a_mailto_beats_a_bare_link() {
        let header = "<https://example.com/u/abc>, <mailto:u@example.com?subject=unsub>";
        assert_eq!(
            parse(Some(header), None),
            Ok(Target::Mail {
                to: vec!["u@example.com".into()],
                subject: "unsub".into(),
                body: None,
            })
        );
    }

    // -------------------------------------------------------------- schemes

    #[test]
    fn only_https_and_mailto_are_allowed() {
        for header in [
            "<javascript:alert(1)>",
            "<data:text/html,hi>",
            "<file:///etc/passwd>",
            "<ftp://example.com/u>",
            "<chrome://settings>",
            "<vbscript:msgbox>",
        ] {
            assert_eq!(
                parse(Some(header), one_click_header()),
                Err(Unusable::BadScheme),
                "{header} must be refused"
            );
        }
    }

    #[test]
    fn plain_http_is_refused_rather_than_upgraded() {
        assert_eq!(
            parse(Some("<http://example.com/u/abc>"), one_click_header()),
            Err(Unusable::BadScheme)
        );
    }

    #[test]
    fn a_scheme_is_matched_case_insensitively() {
        assert_eq!(
            parse(Some("<HTTPS://example.com/u>"), one_click_header()),
            Ok(Target::OneClick {
                url: "https://example.com/u".into()
            })
        );
        assert_eq!(
            parse(Some("<MAILTO:u@example.com>"), None),
            Ok(Target::Mail {
                to: vec!["u@example.com".into()],
                subject: DEFAULT_MAILTO_SUBJECT.into(),
                body: None,
            })
        );
    }

    #[test]
    fn a_usable_target_survives_an_unusable_sibling() {
        let header = "<javascript:alert(1)>, <https://example.com/u/abc>";
        assert_eq!(
            parse(Some(header), one_click_header()),
            Ok(Target::OneClick {
                url: "https://example.com/u/abc".into()
            })
        );
    }

    // ----------------------------------------------------------------- host

    #[test]
    fn a_url_pointing_at_this_machine_is_refused() {
        for header in [
            "<https://127.0.0.1/u>",
            "<https://localhost/u>",
            "<https://LOCALHOST:8443/u>",
            "<https://x.localhost/u>",
            "<https://[::1]/u>",
            "<https://0.0.0.0/u>",
            "<https://[::ffff:127.0.0.1]/u>",
        ] {
            assert_eq!(
                parse(Some(header), one_click_header()),
                Err(Unusable::UnsafeHost),
                "{header} must be refused"
            );
        }
    }

    #[test]
    fn a_url_pointing_into_a_private_network_is_refused() {
        for header in [
            "<https://192.168.1.5/u>",
            "<https://10.0.0.1/u>",
            "<https://172.16.4.4/u>",
            "<https://169.254.1.1/u>",
            "<https://100.64.9.9/u>",
            "<https://[fd00::1]/u>",
            "<https://[fe80::1]/u>",
        ] {
            assert_eq!(
                parse(Some(header), one_click_header()),
                Err(Unusable::UnsafeHost),
                "{header} must be refused"
            );
        }
    }

    #[test]
    fn credentials_in_the_url_are_refused() {
        assert_eq!(
            parse(Some("<https://user:pw@example.com/u>"), one_click_header()),
            Err(Unusable::UnsafeHost)
        );
    }

    #[test]
    fn a_url_longer_than_the_cap_is_refused() {
        let long = format!("<https://example.com/{}>", "a".repeat(MAX_URL_LEN));
        assert_eq!(parse(Some(&long), one_click_header()), Err(Unusable::TooLong));
    }

    // --------------------------------------------------------------- mailto

    #[test]
    fn a_mailto_keeps_its_subject() {
        assert_eq!(
            parse(Some("<mailto:leave-9f2@lists.example.org?subject=unsubscribe%20me>"), None),
            Ok(Target::Mail {
                to: vec!["leave-9f2@lists.example.org".into()],
                subject: "unsubscribe me".into(),
                body: None,
            })
        );
    }

    #[test]
    fn a_mailto_with_no_subject_gets_the_default() {
        assert_eq!(
            parse(Some("<mailto:u@example.com>"), None),
            Ok(Target::Mail {
                to: vec!["u@example.com".into()],
                subject: DEFAULT_MAILTO_SUBJECT.into(),
                body: None,
            })
        );
    }

    #[test]
    fn a_mailto_keeps_a_body_when_one_is_asked_for() {
        assert_eq!(
            parse(Some("<mailto:u@example.com?subject=leave&body=confirm%20token%3D9>"), None),
            Ok(Target::Mail {
                to: vec!["u@example.com".into()],
                subject: "leave".into(),
                body: Some("confirm token=9".into()),
            })
        );
    }

    #[test]
    fn plus_in_a_mailto_query_is_a_plus_and_not_a_space() {
        assert_eq!(
            parse(Some("<mailto:u@example.com?subject=a+b>"), None),
            Ok(Target::Mail {
                to: vec!["u@example.com".into()],
                subject: "a+b".into(),
                body: None,
            })
        );
    }

    #[test]
    fn extra_recipients_in_the_query_are_ignored() {
        // `to`, `cc` and `bcc` are legal RFC 6068 and would let a sender mail a
        // third party from his account.
        let header = "<mailto:u@example.com?subject=leave&cc=victim@elsewhere.com&bcc=x@y.com&to=z@w.com>";
        assert_eq!(
            parse(Some(header), None),
            Ok(Target::Mail {
                to: vec!["u@example.com".into()],
                subject: "leave".into(),
                body: None,
            })
        );
    }

    #[test]
    fn a_newline_in_a_subject_cannot_become_a_second_header() {
        let header = "<mailto:u@example.com?subject=leave%0D%0ABcc:%20victim@elsewhere.com>";
        let Ok(Target::Mail { subject, .. }) = parse(Some(header), None) else {
            panic!("expected a mail target");
        };
        assert!(!subject.contains('\r') && !subject.contains('\n'), "{subject:?}");
        assert_eq!(subject, "leave  Bcc: victim@elsewhere.com");
    }

    #[test]
    fn a_mailto_with_no_address_is_unusable() {
        assert_eq!(parse(Some("<mailto:?subject=leave>"), None), Err(Unusable::NoRecipient));
        assert_eq!(parse(Some("<mailto:not-an-address>"), None), Err(Unusable::NoRecipient));
    }

    #[test]
    fn several_addresses_are_kept_up_to_the_cap() {
        let header = "<mailto:a@x.com,b@x.com,c@x.com,d@x.com,e@x.com>";
        let Ok(Target::Mail { to, .. }) = parse(Some(header), None) else {
            panic!("expected a mail target");
        };
        assert_eq!(to.len(), MAX_MAILTO_RECIPIENTS);
        assert_eq!(to[0], "a@x.com");
    }

    // ------------------------------------------------------------ reporting

    #[test]
    fn the_refusal_names_the_most_specific_problem() {
        // Bad scheme is the least interesting answer; a host refusal outranks it.
        assert_eq!(
            parse(Some("<javascript:x>, <https://10.0.0.1/u>"), None),
            Err(Unusable::UnsafeHost)
        );
    }

    #[test]
    fn every_unusable_has_a_name() {
        use std::collections::BTreeSet;
        let all = [
            Unusable::Empty,
            Unusable::Malformed,
            Unusable::BadScheme,
            Unusable::UnsafeHost,
            Unusable::NoRecipient,
            Unusable::TooLong,
        ];
        let names: BTreeSet<&str> = all.iter().map(|u| u.as_str()).collect();
        assert_eq!(names.len(), all.len(), "two reasons share a name");
    }

    #[test]
    fn only_a_link_needs_a_human() {
        assert!(Target::OneClick { url: "https://x.example/u".into() }.is_automatic());
        assert!(Target::Mail {
            to: vec!["u@x.example".into()],
            subject: "unsubscribe".into(),
            body: None
        }
        .is_automatic());
        assert!(!Target::Link { url: "https://x.example/u".into() }.is_automatic());
    }
}
