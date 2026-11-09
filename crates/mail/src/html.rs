//! Small allowlist HTML sanitizer for composed mail and signatures, plus
//! the plain-text rendering of an HTML body.
//!
//! No HTML parser crate is in the workspace, and the job is narrow
//! (composed mail, not arbitrary web pages), so this is a hand-written
//! single-pass parser over a fixed allowlist instead of a new dependency:
//!
//! * Allowed tags: `p br b strong i em u a ul ol li blockquote h1 h2 h3
//!   span div hr img`. Everything else is stripped; dangerous elements
//!   (`script style iframe object embed form svg title textarea video
//!   audio input button select noscript template link meta base`) are
//!   dropped together with their content, the rest keep their inner text.
//! * Allowed attributes: `a[href]` with an `http`/`https`/`mailto`
//!   scheme only, `img[src]` with an `http`/`https` scheme only. Every
//!   other attribute (including all `on*` handlers, `style`, `class`) is
//!   dropped, so no script can hide in an attribute.
//! * Text is entity-decoded on the way in and re-escaped on the way out,
//!   so `&lt;script&gt;` stays text and never becomes a live tag. Tag and
//!   attribute names are matched case-insensitively and emitted lowercase.
//! * Malformed input degrades to escaped text (`a < b` survives as text);
//!   unclosed tags are passed through and left for the browser to close.
//!
//! `html_to_text` renders the same HTML as plain text for the
//! `multipart/alternative` text part: `br`/`hr` and block boundaries
//! become newlines, everything else is stripped, entities are decoded.

/// Tags whose content is dropped along with the tag itself: code and
/// embedded content, never message text.
const DROP_WITH_CONTENT: [&str; 21] = [
    "script", "style", "iframe", "object", "embed", "form", "svg", "title", "textarea", "video",
    "audio", "input", "button", "select", "noscript", "template", "link", "meta", "base", "math",
    "frame",
];

/// Tags kept as-is (attributes filtered separately).
const ALLOWED: [&str; 19] = [
    "p",
    "br",
    "b",
    "strong",
    "i",
    "em",
    "u",
    "a",
    "ul",
    "ol",
    "li",
    "blockquote",
    "h1",
    "h2",
    "h3",
    "span",
    "div",
    "hr",
    "img",
];

/// Tags that never have a closing tag in the output.
fn is_void(tag: &str) -> bool {
    matches!(tag, "br" | "hr" | "img")
}

/// Block tags: a newline goes after their close tag in `html_to_text`.
fn is_block(tag: &str) -> bool {
    matches!(
        tag,
        "p" | "div" | "li" | "h1" | "h2" | "h3" | "blockquote" | "ul" | "ol" | "tr"
    )
}

/// Decode the entities a browser would decode in text and attribute
/// values: the five named ones, `&nbsp;`, and decimal/hex numeric
/// references. Anything else is left as-is (a stray `&` is literal).
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.find(';').filter(|e| *e <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some('\u{a0}'),
            _ => {
                if let Some(num) = entity
                    .strip_prefix("#x")
                    .or_else(|| entity.strip_prefix("#X"))
                {
                    u32::from_str_radix(num, 16).ok().and_then(char::from_u32)
                } else if let Some(num) = entity.strip_prefix('#') {
                    num.parse::<u32>().ok().and_then(char::from_u32)
                } else {
                    None
                }
            }
        };
        match decoded {
            Some(c) => out.push(c),
            // Unknown entity: keep it verbatim so nothing is silently lost.
            None => out.push_str(&rest[..=end]),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Escape text for re-emitting as HTML: `&` first so nothing already
/// escaped is double-decoded by the reader.
fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

fn escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// The scheme of a URL after trimming and stripping control characters,
/// lowercased. `None` when there is no `scheme:` prefix at all.
fn scheme_of(url: &str) -> Option<String> {
    let cleaned: String = url
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_string();
    let (scheme, _) = cleaned.split_once(':')?;
    if scheme
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        && !scheme.is_empty()
    {
        Some(scheme.to_ascii_lowercase())
    } else {
        None
    }
}

fn url_allowed(url: &str, allow_mailto: bool) -> bool {
    match scheme_of(url).as_deref() {
        Some("http") | Some("https") => true,
        Some("mailto") => allow_mailto,
        // No scheme (a relative URL) or any other scheme is refused:
        // the attribute is dropped and the tag keeps its text.
        _ => false,
    }
}

/// One parsed tag: name, closing or not, and raw attribute text (quotes
/// intact) for the attribute parser.
struct Tag<'a> {
    name: String,
    closing: bool,
    attrs: &'a str,
    self_closing: bool,
    /// Byte index just past the closing `>`, or the input end when the
    /// tag is unterminated.
    end: usize,
}

/// Parse the tag starting at `input[start]` (`<`). Returns `None` when
/// it is not a tag at all (a bare `<`, `<!` without a comment, `<?`),
/// so the caller treats the `<` as text.
fn parse_tag(input: &str, start: usize) -> Option<Tag<'_>> {
    let bytes = input.as_bytes();
    let mut i = start + 1;
    if i >= bytes.len() {
        return None;
    }
    // Comments and declarations are not tags; the caller skips them.
    if bytes[i] == b'!' || bytes[i] == b'?' {
        return None;
    }
    let closing = if bytes[i] == b'/' {
        i += 1;
        true
    } else {
        false
    };
    if i >= bytes.len() || !bytes[i].is_ascii_alphabetic() {
        return None;
    }
    let name_start = i;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric()) {
        i += 1;
    }
    let name = input[name_start..i].to_ascii_lowercase();
    // Attributes run to the first `>` outside quotes.
    let attrs_start = i;
    let mut quote = None;
    while i < bytes.len() {
        let b = bytes[i];
        match (quote, b) {
            (Some(q), b) if b == q => quote = None,
            (None, b'\'') | (None, b'"') => quote = Some(b),
            (None, b'>') => {
                return Some(Tag {
                    name,
                    closing,
                    attrs: &input[attrs_start..i],
                    self_closing: i > attrs_start && input.as_bytes()[i - 1] == b'/',
                    end: i + 1,
                });
            }
            _ => {}
        }
        i += 1;
    }
    // Unterminated tag: everything to the end is attribute text.
    Some(Tag {
        name,
        closing,
        attrs: &input[attrs_start..i],
        self_closing: false,
        end: i,
    })
}

/// The kept attributes for one allowed tag, already escaped. `a` keeps a
/// safe `href`, `img` keeps a safe `src`; every other attribute is gone.
fn kept_attrs(tag: &str, attrs: &str) -> String {
    let wanted = match tag {
        "a" => Some(("href", true)),
        "img" => Some(("src", false)),
        _ => None,
    };
    let Some((want, allow_mailto)) = wanted else {
        return String::new();
    };
    let bytes = attrs.as_bytes();
    let mut i = 0;
    let mut out = String::new();
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let name_start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'='
            && bytes[i] != b'/'
        {
            i += 1;
        }
        if i == name_start {
            break;
        }
        let name = attrs[name_start..i].to_ascii_lowercase();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let q = bytes[i];
                i += 1;
                let v_start = i;
                while i < bytes.len() && bytes[i] != q {
                    i += 1;
                }
                value = attrs[v_start..i].to_string();
                if i < bytes.len() {
                    i += 1;
                }
            } else {
                let v_start = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
                    i += 1;
                }
                value = attrs[v_start..i].to_string();
            }
        }
        if name == want {
            let decoded = decode_entities(&value);
            if url_allowed(&decoded, allow_mailto) {
                out.push(' ');
                out.push_str(&name);
                out.push_str("=\"");
                out.push_str(&escape_attr(decoded.trim()));
                out.push('"');
            }
        }
    }
    out
}

/// Sanitize composed HTML to the allowlist. See the module docs.
pub fn sanitize_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let bytes = input.as_bytes();
    // Name of the drop-with-content element being skipped, if any.
    let mut skipping: Option<String> = None;
    while i < bytes.len() {
        if let Some(drop) = &skipping {
            // Look for the matching close tag; everything before it is code.
            let rest = &input[i..].to_ascii_lowercase();
            let needle = format!("</{drop}");
            if let Some(at) = rest.find(&needle) {
                i += at;
                skipping = None;
                continue;
            }
            break;
        }
        if input[i..].starts_with("<!--") {
            match input[i + 4..].find("-->") {
                Some(end) => i += 4 + end + 3,
                None => break,
            }
            continue;
        }
        if bytes[i] == b'<' {
            match parse_tag(input, i) {
                Some(tag) => {
                    i = tag.end;
                    if tag.closing {
                        if ALLOWED.contains(&tag.name.as_str()) && !is_void(&tag.name) {
                            out.push_str("</");
                            out.push_str(&tag.name);
                            out.push('>');
                        }
                        continue;
                    }
                    if DROP_WITH_CONTENT.contains(&tag.name.as_str()) {
                        if !tag.self_closing {
                            skipping = Some(tag.name);
                        }
                        continue;
                    }
                    if ALLOWED.contains(&tag.name.as_str()) {
                        out.push('<');
                        out.push_str(&tag.name);
                        out.push_str(&kept_attrs(&tag.name, tag.attrs));
                        out.push('>');
                        continue;
                    }
                    // Unknown tag: dropped, its text stays.
                    continue;
                }
                None => {
                    // Not a tag (`<` followed by space, digit, `!`, `?`,
                    // or end of input): literal text.
                    out.push_str("&lt;");
                    i += 1;
                    continue;
                }
            }
        }
        let next = input[i..].find('<').map(|at| i + at).unwrap_or(bytes.len());
        out.push_str(&escape_text(&decode_entities(&input[i..next])));
        i = next;
    }
    out
}

/// Render HTML as plain text: block boundaries and `br`/`hr` become
/// newlines, tags are stripped, entities decoded. Used for the
/// `text/plain` part of a composed HTML message.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut i = 0;
    let bytes = html.as_bytes();
    let mut skipping: Option<String> = None;
    while i < bytes.len() {
        if let Some(drop) = &skipping {
            let rest = &html[i..].to_ascii_lowercase();
            let needle = format!("</{drop}");
            if let Some(at) = rest.find(&needle) {
                i += at;
                skipping = None;
                continue;
            }
            break;
        }
        if html[i..].starts_with("<!--") {
            match html[i + 4..].find("-->") {
                Some(end) => i += 4 + end + 3,
                None => break,
            }
            continue;
        }
        if bytes[i] == b'<' {
            match parse_tag(html, i) {
                Some(tag) => {
                    i = tag.end;
                    if tag.closing {
                        if is_block(&tag.name) {
                            out.push('\n');
                        }
                        continue;
                    }
                    if DROP_WITH_CONTENT.contains(&tag.name.as_str()) {
                        if !tag.self_closing {
                            skipping = Some(tag.name);
                        }
                        continue;
                    }
                    if tag.name == "br" || tag.name == "hr" {
                        out.push('\n');
                    }
                    continue;
                }
                None => {
                    out.push('<');
                    i += 1;
                    continue;
                }
            }
        }
        let next = html[i..].find('<').map(|at| i + at).unwrap_or(bytes.len());
        out.push_str(&decode_entities(&html[i..next]));
        i = next;
    }
    // Collapse runs of blank lines, then trim the edges.
    let mut cleaned = String::with_capacity(out.len());
    let mut blanks = 0;
    for line in out.split('\n') {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            blanks += 1;
            if blanks <= 1 {
                cleaned.push('\n');
            }
        } else {
            blanks = 0;
            if !cleaned.is_empty() && !cleaned.ends_with('\n') {
                cleaned.push('\n');
            }
            cleaned.push_str(trimmed);
        }
    }
    cleaned.trim().to_string()
}

/// Render plain text as simple HTML: escaped, one `<br>` per line break.
/// Used to derive the HTML form of a signature typed as plain text.
pub fn text_to_html(text: &str) -> String {
    escape_text(text).replace('\n', "<br>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_script_and_style_with_content() {
        assert_eq!(
            sanitize_html("<p>hi</p><script>alert(1)</script><style>p{color:red}</style>bye"),
            "<p>hi</p>bye"
        );
        assert_eq!(sanitize_html("<SCRIPT SRC=x></SCRIPT>ok"), "ok");
        assert_eq!(sanitize_html("<script>unclosed"), "");
    }

    #[test]
    fn strips_event_handlers_and_unknown_tags() {
        assert_eq!(
            sanitize_html("<p onclick=\"evil()\" style=\"color:red\">x</p>"),
            "<p>x</p>"
        );
        assert_eq!(
            sanitize_html("<div><table><tr><td>cell</td></tr></table></div>"),
            "<div>cell</div>"
        );
        assert_eq!(
            sanitize_html("<b onmouseover=alert(1)>bold</b>"),
            "<b>bold</b>"
        );
    }

    #[test]
    fn strips_iframe_object_form_and_svg() {
        assert_eq!(
            sanitize_html("<iframe src=\"https://x.id\"></iframe>after"),
            "after"
        );
        assert_eq!(
            sanitize_html("<object data=\"x\">inner</object>after"),
            "after"
        );
        assert_eq!(
            sanitize_html("<form action=\"/x\"><input></form>after"),
            "after"
        );
        assert_eq!(sanitize_html("<svg><circle/></svg>after"), "after");
    }

    #[test]
    fn link_schemes() {
        assert_eq!(
            sanitize_html("<a href=\"https://x.id\">ok</a>"),
            "<a href=\"https://x.id\">ok</a>"
        );
        assert_eq!(
            sanitize_html("<a href=\"mailto:a@x.id\">mail</a>"),
            "<a href=\"mailto:a@x.id\">mail</a>"
        );
        assert_eq!(
            sanitize_html("<a href=\"javascript:alert(1)\">x</a>"),
            "<a>x</a>"
        );
        assert_eq!(
            sanitize_html("<a href=\"JaVaScRipT:alert(1)\">x</a>"),
            "<a>x</a>"
        );
        assert_eq!(
            sanitize_html("<a href=\"/relative/path\">x</a>"),
            "<a>x</a>"
        );
        assert_eq!(
            sanitize_html("<a href=\"java\tscript:alert(1)\">x</a>"),
            "<a>x</a>"
        );
    }

    #[test]
    fn image_src_rules() {
        assert_eq!(
            sanitize_html("<img src=\"https://x.id/p.png\" onerror=\"alert(1)\">"),
            "<img src=\"https://x.id/p.png\">"
        );
        assert_eq!(
            sanitize_html("<img src=\"data:image/png;base64,AAA\">"),
            "<img>"
        );
        assert_eq!(sanitize_html("<img src=\"/local.png\">"), "<img>");
    }

    #[test]
    fn entities_never_become_live_tags() {
        assert_eq!(
            sanitize_html("&lt;script&gt;alert(1)&lt;/script&gt;"),
            "&lt;script&gt;alert(1)&lt;/script&gt;"
        );
        assert_eq!(sanitize_html("a &amp; b &lt; c"), "a &amp; b &lt; c");
    }

    #[test]
    fn keeps_allowed_formatting_and_lists() {
        let html = "<h1>T</h1><p><b>bold</b> <i>it</i> <u>u</u></p><ul><li>one</li><li>two</li></ul><blockquote>q</blockquote><hr>";
        assert_eq!(sanitize_html(html), html);
    }

    #[test]
    fn malformed_input_degrades_to_text() {
        assert_eq!(sanitize_html("a < b and c > d"), "a &lt; b and c &gt; d");
        assert_eq!(sanitize_html("<b>unclosed"), "<b>unclosed");
        assert_eq!(sanitize_html("<!-- secret -->visible"), "visible");
        assert_eq!(sanitize_html("<p title=\"a>b\">x</p>"), "<p>x</p>");
    }

    #[test]
    fn html_to_text_renders_blocks_and_breaks() {
        assert_eq!(html_to_text("<p>Hello</p><p>World</p>"), "Hello\nWorld");
        assert_eq!(html_to_text("one<br>two<br/>three"), "one\ntwo\nthree");
        assert_eq!(
            html_to_text("<ul><li>one</li><li>two</li></ul>"),
            "one\ntwo"
        );
        assert_eq!(html_to_text("<p>a &amp; b</p>"), "a & b");
        assert_eq!(html_to_text("<script>evil()</script>kept"), "kept");
    }

    #[test]
    fn text_to_html_escapes_and_breaks() {
        assert_eq!(text_to_html("a < b\nline two"), "a &lt; b<br>line two");
    }
}
