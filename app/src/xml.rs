/// Escape text for use inside XML element content or a double-quoted attribute value.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// RFC 822 date, the form Newznab `pubDate` elements use.
pub fn rfc822(epoch: i64) -> String {
    use chrono::{DateTime, Utc};
    let dt = DateTime::<Utc>::from_timestamp(epoch, 0).unwrap_or(DateTime::UNIX_EPOCH);
    dt.format("%a, %d %b %Y %H:%M:%S %z").to_string()
}
