use super::{DebugReport, GITHUB_ISSUES_NEW_URL};

pub(super) const MAX_ISSUE_URL_BYTES: usize = 6500;
const TRUNCATION_NOTE: &str =
    "\n\n(Truncated for URL length — paste full output from 'zensmu report' here)";

impl DebugReport {
    /// Build a pre-filled GitHub "New Issue" URL with RFC 3986 percent-encoding.
    ///
    /// Guarantees the returned URL stays within `6500` bytes by cleanly
    /// truncating the encoded `body` (preserving the summary and errors) and
    /// appending a truncation note when necessary.
    pub fn github_issue_url(&self) -> String {
        let title = self.suggested_issue_title();
        let labels = self.suggested_labels().join(",");
        let prefix = format!(
            "{GITHUB_ISSUES_NEW_URL}?title={}&labels={}&body=",
            percent_encode(&title),
            percent_encode(&labels)
        );

        if prefix.len() >= MAX_ISSUE_URL_BYTES {
            // Pathological title fallback: cap at MAX_ISSUE_URL_BYTES on a valid boundary.
            return truncate_url_prefix(&prefix, MAX_ISSUE_URL_BYTES);
        }

        let body_budget = MAX_ISSUE_URL_BYTES - prefix.len();
        let body = self.to_markdown();
        let encoded_full = percent_encode(&body);

        if encoded_full.len() <= body_budget {
            let mut url = String::with_capacity(prefix.len() + encoded_full.len());
            url.push_str(&prefix);
            url.push_str(&encoded_full);
            return url;
        }

        let encoded_truncated = encode_truncated_body(&body, body_budget);
        let mut url = String::with_capacity(prefix.len() + encoded_truncated.len());
        url.push_str(&prefix);
        url.push_str(&encoded_truncated);
        url
    }
}

/// Zero-dependency RFC 3986 percent-encoder (`ALPHA / DIGIT / "-" / "." / "_" / "~"` unreserved).
pub fn percent_encode(input: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        if is_rfc3986_unreserved(byte) {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0F) as usize] as char);
        }
    }
    out
}

fn is_rfc3986_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

fn encoded_char_len(ch: char) -> usize {
    let mut buf = [0u8; 4];
    let bytes = ch.encode_utf8(&mut buf).as_bytes();
    bytes
        .iter()
        .map(|&b| if is_rfc3986_unreserved(b) { 1 } else { 3 })
        .sum()
}

fn encode_truncated_body(body: &str, budget: usize) -> String {
    let encoded_note = percent_encode(TRUNCATION_NOTE);
    if budget <= encoded_note.len() {
        return truncate_encoded_to_budget(TRUNCATION_NOTE, budget);
    }

    let content_budget = budget - encoded_note.len();
    let mut enc_len = 0usize;
    let mut cut_idx = 0usize;
    let mut last_nl = None;

    for (idx, ch) in body.char_indices() {
        let ch_len = encoded_char_len(ch);
        if enc_len + ch_len > content_budget {
            break;
        }
        enc_len += ch_len;
        cut_idx = idx + ch.len_utf8();
        if ch == '\n' {
            last_nl = Some(idx);
        }
    }

    let mut slice = match last_nl {
        Some(nl) if nl >= cut_idx / 2 => &body[..nl],
        _ => &body[..cut_idx],
    };

    // If truncation fell inside an unclosed `<details>` section, back up to before
    // `<details>` so the summary + errors stay intact and the note is visible at top level.
    if let Some(open_pos) = slice.rfind("<details>") {
        if !slice[open_pos..].contains("</details>") && open_pos > 0 {
            slice = slice[..open_pos].trim_end();
        }
    }

    let mut out = percent_encode(slice);
    out.push_str(&encoded_note);
    out
}

fn truncate_encoded_to_budget(text: &str, budget: usize) -> String {
    let mut enc_len = 0usize;
    let mut cut_idx = 0usize;
    for (idx, ch) in text.char_indices() {
        let ch_len = encoded_char_len(ch);
        if enc_len + ch_len > budget {
            break;
        }
        enc_len += ch_len;
        cut_idx = idx + ch.len_utf8();
    }
    percent_encode(&text[..cut_idx])
}

fn truncate_url_prefix(prefix: &str, max_len: usize) -> String {
    let mut end = max_len.min(prefix.len());
    // Avoid splitting an ASCII `%XX` escape sequence.
    if end >= 1 && prefix.as_bytes()[end - 1] == b'%' {
        end -= 1;
    } else if end >= 2 && prefix.as_bytes()[end - 2] == b'%' {
        end -= 2;
    }
    prefix[..end].to_string()
}
