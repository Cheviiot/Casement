//! Clipboard text between systems: the network carries "\n" line ends and at most
//! MAX_CLIPBOARD_TEXT bytes; Windows wants "\r\n" and UTF-16 with a closing zero.
#![cfg_attr(not(windows), allow(dead_code))]

/// UTF-16 with "\r\n" line ends and the closing zero, as CF_UNICODETEXT holds it.
pub(crate) fn to_windows(text: &str) -> Vec<u16> {
    let mut out = Vec::with_capacity(text.len() + 1);
    let mut previous = '\0';
    for c in text.chars() {
        if c == '\n' && previous != '\r' {
            out.push('\r' as u16);
        }
        let mut buf = [0u16; 2];
        out.extend_from_slice(c.encode_utf16(&mut buf));
        previous = c;
    }
    out.push(0);
    out
}

/// From CF_UNICODETEXT (up to its zero) to "\n" line ends, cut to what the protocol takes.
pub(crate) fn from_windows(units: &[u16]) -> String {
    let len = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    let text = String::from_utf16_lossy(&units[..len]).replace("\r\n", "\n");
    casement_net::client::clip_text(&text).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_ends_and_zero() {
        let units = to_windows("a\nб\r\nc");
        assert_eq!(String::from_utf16(&units).unwrap(), "a\r\nб\r\nc\0");
        assert_eq!(from_windows(&units), "a\nб\nc");
        assert_eq!(from_windows(&to_windows("😀")), "😀");
        assert_eq!(from_windows(&[0x41, 0, 0x42]), "A");
    }
}
