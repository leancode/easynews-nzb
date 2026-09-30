use crate::models::Ticket;
use crate::xml::escape;

const POSTER: &str = "easynews-nzb";

/// The "ticket NZB": a minimal, valid NZB (root `<nzb>` with one `<file>`/`<segment>`) that only
/// Radarr/Sonarr/Prowlarr need to accept as a download; see docs/DESIGN.md section 4.
pub fn build_nzb(ticket: &Ticket) -> String {
    let title = escape(&ticket.name);
    let bytes: u64 = ticket.files.iter().map(|f| f.size).sum();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE nzb PUBLIC "-//newzBin//DTD NZB 1.1//EN" "http://www.newzbin.com/DTD/nzb/nzb-1.1.dtd">
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
  <head>
    <meta type="title">{title}</meta>
    <meta type="x-easynews-ticket">{token}</meta>
  </head>
  <file poster="{poster}" date="{date}" subject="{title}">
    <groups><group>alt.binaries.misc</group></groups>
    <segments><segment bytes="{bytes}" number="1">{token}@{poster}</segment></segments>
  </file>
</nzb>
"#,
        token = escape(&ticket.token),
        date = ticket.created,
        poster = POSTER,
    )
}

/// Pull the `x-easynews-ticket` meta value out of an uploaded NZB, if this is one of ours.
pub fn extract_ticket_token(nzb_xml: &str) -> Option<String> {
    let marker = r#"<meta type="x-easynews-ticket">"#;
    let start = nzb_xml.find(marker)? + marker.len();
    let end = nzb_xml[start..].find("</meta>")? + start;
    Some(nzb_xml[start..end].trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_ticket_token() {
        let ticket = Ticket {
            token: "abc123".into(),
            name: "Some <Release> & Title".into(),
            category: "2040".into(),
            files: vec![],
            created: 1000,
            refresh_query: "q".into(),
            refresh_types: vec![],
        };
        let xml = build_nzb(&ticket);
        assert_eq!(extract_ticket_token(&xml).as_deref(), Some("abc123"));
        assert!(xml.contains("Some &lt;Release&gt; &amp; Title"));
    }
}
