//! E1 replay envelope helpers: find end of Extension XML so we can split off BcMedia payload.
//! Handles </Extension>\n, </Extension>\r\n, </Extension> (no newline), and fallback to first "00dc" (BcMedia magic).

/// BcMedia video magic (ASCII "00dc" LE) – fallback to find media start when XML closing varies.
const BCMEDIA_MAGIC_00DC: &[u8] = b"00dc";

/// Find the byte offset of the first byte *after* the E1 Extension XML (start of media payload).
/// Tries: `</Extension>` (case-insensitive) then optional `\n` or `\r\n`; then fallback: first "00dc".
/// `search_from` is the offset in `data` at which XML starts (e.g. 0 or 32 if 32-byte prefix).
/// Returns None if no end found; otherwise Some(offset) with offset <= data.len().
pub(crate) fn e1_xml_end_offset(data: &[u8], search_from: usize) -> Option<usize> {
    if search_from >= data.len() {
        return None;
    }
    let tail = &data[search_from..];
    let first_00dc = tail
        .windows(BCMEDIA_MAGIC_00DC.len())
        .position(|w| w == BCMEDIA_MAGIC_00DC)
        .map(|i| search_from + i);

    const EXT_END: &[u8] = b"</Extension>";
    // Match </Extension> case-insensitively so </extension> or </EXTENSION> from camera still work.
    let ext_tag_pos = tail
        .windows(EXT_END.len())
        .position(|w| w.eq_ignore_ascii_case(EXT_END));

    let payload_start = match ext_tag_pos {
        Some(pos) => {
            let after_tag = search_from + pos + EXT_END.len();
            let mut ps = after_tag;
            while ps < data.len() && (data[ps] == b'\n' || data[ps] == b'\r') {
                ps += 1;
            }
            // If "00dc" appears before this, use it so we never skip into media.
            match first_00dc {
                Some(off) if off < ps => off,
                _ => ps,
            }
        }
        None => {
            // No </Extension> — use first "00dc" as media start if present (some cameras omit tag).
            return first_00dc;
        }
    };
    Some(payload_start)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn e1_xml_end_offset_case_insensitive() {
        // </Extension> (canonical)
        let a = b"<?xml version=\"1.0\"?>\n<Extension></Extension>\n00dc";
        assert_eq!(e1_xml_end_offset(a, 0), Some(46)); // after \n
        // </extension> (lowercase) — camera may send this
        let b = b"<?xml version=\"1.0\"?>\n<Extension></extension>\n00dc";
        assert_eq!(e1_xml_end_offset(b, 0), Some(46));
        // </EXTENSION>
        let c = b"<?xml version=\"1.0\"?>\n<Extension></EXTENSION>\r\n00dc";
        assert_eq!(e1_xml_end_offset(c, 0), Some(47));
    }

    #[test]
    fn e1_xml_end_offset_no_tag_uses_00dc() {
        let d = b"<?xml version=\"1.0\"?>\n<foo>bar</foo>00dcH264";
        assert_eq!(e1_xml_end_offset(d, 0), Some(36)); // first 00dc
        let e = b"no xml 00dc";
        assert_eq!(e1_xml_end_offset(e, 0), Some(7)); // "00dc" at index 7
    }
}
