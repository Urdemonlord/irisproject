//! DMARC aggregate reports (RFC 7489): the XML a receiving mail server
//! sends back about mail it got for one of your domains.
//!
//! The mail crate already extracts the report XML text from an attachment
//! (`extract_text_attachment`: gzip / zip / plain XML). This module is the
//! first real parser of that text: it turns one `<feedback>` document into
//! a `DmarcReport` of per-source rows. It parses only what the dashboard
//! shows: reporting org, report id, published domain, date range, and for
//! each `<record>` the source IP, message count, disposition, and the
//! *aligned* DKIM / SPF results from `<policy_evaluated>`.
//!
//! Parsing is intentionally strict about shape and lenient about content:
//! a document that is not a `<feedback>` report, has no domain or report
//! id, or has no usable record is an error; a single malformed record is
//! skipped instead of failing the whole report. Unknown elements are
//! ignored, so a future report version with extra fields still parses.
use crate::MailError;
use quick_xml::events::Event;
use quick_xml::reader::Reader;

/// One `<record>`: one source IP's traffic for the report window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmarcRecord {
    pub source_ip: String,
    pub count: u64,
    /// `none`, `quarantine`, or `reject` (empty when the report omits it).
    pub disposition: String,
    /// Aligned DKIM result from `<policy_evaluated>`: `pass`, `fail`, ...
    pub dkim_result: String,
    /// Aligned SPF result from `<policy_evaluated>`: `pass`, `fail`, ...
    pub spf_result: String,
}

/// DMARC passes for a record when either aligned check passed: that is
/// what "aligned" means in the report, and what the dashboard counts.
pub fn dmarc_passed(r: &DmarcRecord) -> bool {
    r.dkim_result.eq_ignore_ascii_case("pass") || r.spf_result.eq_ignore_ascii_case("pass")
}

/// One parsed aggregate report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmarcReport {
    pub org_name: String,
    pub report_id: String,
    pub domain: String,
    /// Unix seconds, from `<date_range>`.
    pub date_begin: i64,
    pub date_end: i64,
    pub records: Vec<DmarcRecord>,
}

fn err(msg: &str) -> MailError {
    MailError::Protocol(format!("dmarc: {msg}"))
}

/// Resolves one entity reference (`&amp;`, `&#38;`, `&#x26;`) to its
/// character. Unknown named entities resolve to nothing: a report that
/// uses a custom DTD entity is vanishingly rare, and dropping one
/// character beats failing the whole report.
fn resolve_entity(name: &str) -> String {
    match name {
        "amp" => "&".to_string(),
        "lt" => "<".to_string(),
        "gt" => ">".to_string(),
        "quot" => "\"".to_string(),
        "apos" => "'".to_string(),
        _ => {
            let code = name
                .strip_prefix("#x")
                .or_else(|| name.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| name.strip_prefix('#').and_then(|d| d.parse::<u32>().ok()));
            code.and_then(char::from_u32)
                .map(|c| c.to_string())
                .unwrap_or_default()
        }
    }
}

/// Parses one DMARC aggregate report document.
///
/// The parser walks the XML once with a small element stack. Text is only
/// kept for tags the dashboard reads, in the section they belong to, so a
/// `<domain>` under `<auth_results>` never becomes the published domain.
pub fn parse_aggregate_report(xml: &str) -> Result<DmarcReport, MailError> {
    let mut reader = Reader::from_str(xml);
    // No trim_text: an entity (`&amp;`) splits one element's text into
    // several Text events, and trimming each chunk would eat the spaces
    // around it ("google & friends" would become "google&friends").
    // Whitespace-only chunks (indentation) are skipped in the handler
    // and the accumulated values are trimmed when finalized instead.
    let mut buf = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut root_seen = false;

    let mut org_name = String::new();
    let mut report_id = String::new();
    let mut domain = String::new();
    let mut date_begin: i64 = 0;
    let mut date_end: i64 = 0;
    let mut records: Vec<DmarcRecord> = Vec::new();
    let mut cur: Option<DmarcRecord> = None;

    // Where a piece of text belongs: the innermost named section.
    let section = |stack: &[String]| -> &str {
        if stack.iter().any(|t| t == "record") {
            "record"
        } else if stack.iter().any(|t| t == "policy_published") {
            "policy"
        } else if stack.iter().any(|t| t == "date_range") {
            "dates"
        } else if stack.iter().any(|t| t == "report_metadata") {
            "meta"
        } else {
            ""
        }
    };

    loop {
        // Text and entity references both carry characters: a Text event
        // is one run of characters, a GeneralRef event is one `&entity;`
        // (quick-xml splits them apart), so both feed the same append
        // step below instead of two copies of it. The chunk is owned:
        // it borrows the read buffer, which is cleared each iteration.
        let chunk: Option<String> = match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.name().as_ref().to_owned();
                if !root_seen {
                    if name != "feedback" {
                        return Err(err("not a DMARC aggregate report"));
                    }
                    root_seen = true;
                }
                if name == "record" {
                    cur = Some(DmarcRecord {
                        source_ip: String::new(),
                        count: 0,
                        disposition: String::new(),
                        dkim_result: String::new(),
                        spf_result: String::new(),
                    });
                }
                stack.push(name);
                None
            }
            Ok(Event::Empty(_)) => None,
            Ok(Event::Text(e)) => Some(e.xml10_content().into_owned()),
            Ok(Event::GeneralRef(e)) => Some(resolve_entity(&e.into_inner())),
            Ok(Event::End(e)) => {
                let name = e.name().as_ref().to_owned();
                if name == "record" {
                    if let Some(mut r) = cur.take() {
                        // Trim what the chunks accumulated (leading and
                        // trailing whitespace only; inner spaces stay).
                        r.source_ip = r.source_ip.trim().to_string();
                        r.disposition = r.disposition.trim().to_string();
                        r.dkim_result = r.dkim_result.trim().to_string();
                        r.spf_result = r.spf_result.trim().to_string();
                        // A record without a source IP is not attributable
                        // to anything; drop it, keep the report.
                        if !r.source_ip.is_empty() {
                            records.push(r);
                        }
                    }
                }
                stack.pop();
                None
            }
            Ok(Event::Eof) => break,
            Ok(_) => None,
            Err(e) => return Err(err(&format!("malformed XML: {e}"))),
        };
        buf.clear();
        let Some(text) = chunk else { continue };
        if text.trim().is_empty() {
            continue;
        }
        let tag = stack.last().map(String::as_str).unwrap_or("");
        // Text arrives in chunks (one per entity), so values are
        // appended, never overwritten. Numbers only move when the chunk
        // parses, so a stray entity inside one cannot zero it.
        match (section(&stack), tag) {
            ("meta", "org_name") => org_name.push_str(&text),
            ("meta", "report_id") => report_id.push_str(&text),
            ("dates", "begin") => {
                if let Ok(v) = text.trim().parse() {
                    date_begin = v;
                }
            }
            ("dates", "end") => {
                if let Ok(v) = text.trim().parse() {
                    date_end = v;
                }
            }
            ("policy", "domain") => domain.push_str(&text),
            ("record", "source_ip") => {
                if let Some(r) = cur.as_mut() {
                    r.source_ip.push_str(&text);
                }
            }
            ("record", "count") => {
                if let Some(r) = cur.as_mut() {
                    if let Ok(v) = text.trim().parse() {
                        r.count = v;
                    }
                }
            }
            ("record", "disposition") => {
                if let Some(r) = cur.as_mut() {
                    r.disposition.push_str(&text);
                }
            }
            ("record", "dkim") | ("record", "spf") => {
                // The *aligned* results live directly under
                // `<policy_evaluated>`: `<dkim>pass</dkim>`. The
                // same tag names under `<auth_results>` carry a
                // `<result>` child instead, and are not aligned
                // results, so they are ignored here.
                let parent = stack.get(stack.len().saturating_sub(2)).map(String::as_str);
                if parent == Some("policy_evaluated") {
                    if let Some(r) = cur.as_mut() {
                        if tag == "dkim" {
                            r.dkim_result.push_str(&text);
                        } else {
                            r.spf_result.push_str(&text);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    if !root_seen {
        return Err(err("empty document"));
    }
    let org_name = org_name.trim().to_string();
    let report_id = report_id.trim().to_string();
    let domain = domain.trim().to_string();
    if report_id.is_empty() {
        return Err(err("missing report_id"));
    }
    if domain.is_empty() {
        return Err(err("missing published domain"));
    }
    if records.is_empty() {
        return Err(err("no usable records"));
    }
    Ok(DmarcReport {
        org_name,
        report_id,
        domain,
        date_begin,
        date_end,
        records,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One Google-style aggregate report: two records, one passing, one
    /// failing, plus the parts the parser must ignore (auth_results,
    /// policy_published extras).
    fn google_report() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8" ?>
<feedback>
  <report_metadata>
    <org_name>google.com</org_name>
    <email>noreply-dmarc-support@google.com</email>
    <report_id>123456789</report_id>
    <date_range>
      <begin>1761705600</begin>
      <end>1761792000</end>
    </date_range>
  </report_metadata>
  <policy_published>
    <domain>example.com</domain>
    <adkim>r</adkim>
    <aspf>r</aspf>
    <p>none</p>
    <sp>none</sp>
    <pct>100</pct>
  </policy_published>
  <record>
    <row>
      <source_ip>192.0.2.10</source_ip>
      <count>42</count>
      <policy_evaluated>
        <disposition>none</disposition>
        <dkim>pass</dkim>
        <spf>pass</spf>
      </policy_evaluated>
    </row>
    <identifiers><header_from>example.com</header_from></identifiers>
    <auth_results>
      <dkim><domain>example.com</domain><result>pass</result></dkim>
      <spf><domain>example.com</domain><result>pass</result></spf>
    </auth_results>
  </record>
  <record>
    <row>
      <source_ip>203.0.113.7</source_ip>
      <count>3</count>
      <policy_evaluated>
        <disposition>quarantine</disposition>
        <dkim>fail</dkim>
        <spf>fail</spf>
      </policy_evaluated>
    </row>
    <identifiers><header_from>example.com</header_from></identifiers>
    <auth_results>
      <dkim><domain>evil.example</domain><result>fail</result></dkim>
      <spf><domain>example.com</domain><result>fail</result></spf>
    </auth_results>
  </record>
</feedback>"#
    }

    #[test]
    fn parses_google_style_report() {
        let r = parse_aggregate_report(google_report()).unwrap();
        assert_eq!(r.org_name, "google.com");
        assert_eq!(r.report_id, "123456789");
        assert_eq!(r.domain, "example.com");
        assert_eq!(r.date_begin, 1761705600);
        assert_eq!(r.date_end, 1761792000);
        assert_eq!(r.records.len(), 2);
        let (a, b) = (&r.records[0], &r.records[1]);
        assert_eq!(a.source_ip, "192.0.2.10");
        assert_eq!(a.count, 42);
        assert_eq!(a.disposition, "none");
        assert_eq!(a.dkim_result, "pass");
        assert_eq!(a.spf_result, "pass");
        assert!(dmarc_passed(a));
        assert_eq!(b.source_ip, "203.0.113.7");
        assert_eq!(b.count, 3);
        assert_eq!(b.disposition, "quarantine");
        assert!(!dmarc_passed(b));
    }

    #[test]
    fn non_feedback_document_is_rejected() {
        assert!(parse_aggregate_report("<html><body>hi</body></html>").is_err());
        assert!(parse_aggregate_report("").is_err());
        assert!(parse_aggregate_report("not xml at all").is_err());
    }

    #[test]
    fn missing_domain_or_report_id_is_rejected() {
        let no_domain = google_report().replace("<domain>example.com</domain>", "");
        assert!(parse_aggregate_report(&no_domain).is_err());
        let no_id = google_report().replace("<report_id>123456789</report_id>", "");
        assert!(parse_aggregate_report(&no_id).is_err());
    }

    #[test]
    fn record_without_source_ip_is_skipped_not_fatal() {
        let xml = google_report().replace(
            "<source_ip>203.0.113.7</source_ip>",
            "<source_ip></source_ip>",
        );
        let r = parse_aggregate_report(&xml).unwrap();
        assert_eq!(r.records.len(), 1);
        assert_eq!(r.records[0].source_ip, "192.0.2.10");
    }

    #[test]
    fn all_records_unusable_is_rejected() {
        let xml = google_report()
            .replace(
                "<source_ip>192.0.2.10</source_ip>",
                "<source_ip></source_ip>",
            )
            .replace(
                "<source_ip>203.0.113.7</source_ip>",
                "<source_ip></source_ip>",
            );
        assert!(parse_aggregate_report(&xml).is_err());
    }

    #[test]
    fn malformed_xml_is_rejected() {
        assert!(parse_aggregate_report("<feedback><report_metadata>").is_err());
    }

    #[test]
    fn entities_are_decoded() {
        let xml = google_report().replace("google.com", "google &amp; friends");
        let r = parse_aggregate_report(&xml).unwrap();
        assert_eq!(r.org_name, "google & friends");
    }

    #[test]
    fn policy_evaluated_results_win_over_auth_results() {
        // auth_results says pass while policy_evaluated says fail: the
        // dashboard counts the aligned (policy) result, so this record
        // must fail even though auth_results disagrees.
        let xml = google_report().replace(
            "<disposition>none</disposition>\n        <dkim>pass</dkim>\n        <spf>pass</spf>",
            "<disposition>none</disposition>\n        <dkim>fail</dkim>\n        <spf>fail</spf>",
        );
        let r = parse_aggregate_report(&xml).unwrap();
        assert!(!dmarc_passed(&r.records[0]));
        assert_eq!(r.records[0].dkim_result, "fail");
    }

    #[test]
    fn numeric_character_references_resolve() {
        let xml = google_report().replace("google.com", "google &#38; friends");
        let r = parse_aggregate_report(&xml).unwrap();
        assert_eq!(r.org_name, "google & friends");
        let xml = google_report().replace("google.com", "google &#x26; friends");
        let r = parse_aggregate_report(&xml).unwrap();
        assert_eq!(r.org_name, "google & friends");
    }
}
