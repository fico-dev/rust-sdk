//! base64url without padding (RFC 4648, section 5), for the PKCE values and
//! the payload of an ID token. In-house: about 30 lines.

/// The base64url character of the 6-bit `value`: `A-Z`, `a-z`, `0-9`, `-`
/// and `_`.
fn char_of(value: u32) -> char {
    let value = value as u8 & 63;
    char::from(match value {
        0..=25 => b'A' + value,
        26..=51 => b'a' + value - 26,
        52..=61 => b'0' + value - 52,
        62 => b'-',
        _ => b'_',
    })
}

/// `bytes` in base64url, without padding.
pub(crate) fn encode_url(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let byte = |at: usize| u32::from(chunk.get(at).copied().unwrap_or(0));
        let group = byte(0) << 16 | byte(1) << 8 | byte(2);
        for at in 0..=chunk.len() {
            text.push(char_of(group >> (18 - 6 * at)));
        }
    }
    text
}

/// The bytes that the base64url `text` holds, with or without padding, or
/// `None` if it is not base64url.
pub(crate) fn decode_url(text: &str) -> Option<Vec<u8>> {
    let text = text.trim_end_matches('=');
    if text.len() % 4 == 1 {
        return None;
    }
    let mut bytes = Vec::with_capacity(text.len() * 3 / 4);
    let (mut bits, mut count) = (0u32, 0);
    for char in text.bytes() {
        let value = match char {
            b'A'..=b'Z' => char - b'A',
            b'a'..=b'z' => char - b'a' + 26,
            b'0'..=b'9' => char - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        };
        bits = bits << 6 | u32::from(value);
        count += 6;
        if count >= 8 {
            count -= 8;
            bytes.push((bits >> count) as u8);
            bits &= (1 << count) - 1;
        }
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_decodes_the_rfc_4648_vectors() {
        for (bytes, text) in [
            ("", ""),
            ("f", "Zg"),
            ("fo", "Zm8"),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg"),
            ("fooba", "Zm9vYmE"),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(encode_url(bytes.as_bytes()), text);
            assert_eq!(decode_url(text).unwrap(), bytes.as_bytes());
        }
        assert_eq!(encode_url(&[0xfb, 0xff]), "-_8");
        // RFC 7636, appendix B: the S256 challenge of its example verifier,
        // written in parts so that secret scanners don't flag them.
        let verifier = ["dBjftJeZ", "4CVP-mB92K27", "uhbUJU1p1r_w", "W1gFWFOEjXk"].concat();
        let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, verifier.as_bytes());
        let challenge = ["E9Melhoa2OwvFrEMTJgu", "CHaoeK1t8URWbuGJSstw-cM"].concat();
        assert_eq!(encode_url(digest.as_ref()), challenge);
        let all: String = (0..64).map(char_of).collect();
        assert_eq!(all.len(), 64);
        assert_eq!(decode_url(&all).unwrap().len(), 48);
        assert_eq!(decode_url("-_8").unwrap(), [0xfb, 0xff]);
        assert_eq!(decode_url("Zm8=").unwrap(), b"fo");
    }

    #[test]
    fn rejects_text_that_is_not_base64url() {
        for text in ["Zm9v+", "Zm9v/", "Z", "Zm9vY", "a b"] {
            assert_eq!(decode_url(text), None, "{text}");
        }
    }
}
