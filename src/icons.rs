//! Site icons: small PNGs the page script hands over, kept per host beside the history
//! file, with a themed monogram when a site has none.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// Longest `data:` URL the bridge accepts; a 32×32 PNG is a few KB.
pub const MAX_DATA_URL: usize = 64 * 1024;
const MAX_SIDE: u32 = 256;
const PREFIX: &str = "data:image/png;base64,";

/// Validated PNG bytes from the page's `data:image/png;base64,…` message, if sane.
pub fn png_from_data_url(data: &str) -> Option<Vec<u8>> {
    if data.len() > MAX_DATA_URL {
        return None;
    }
    let bytes = base64_decode(data.strip_prefix(PREFIX)?)?;
    valid_png(&bytes).then_some(bytes)
}

/// Standard alphabet with `=` padding; anything else is rejected.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let text = text.trim_end_matches('=');
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0;
    for c in text.bytes() {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// PNG signature plus an IHDR with modest dimensions, so the decoder never sees a bomb.
pub fn valid_png(bytes: &[u8]) -> bool {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < 33 || &bytes[..8] != SIGNATURE || &bytes[12..16] != b"IHDR" {
        return false;
    }
    let side = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
    (1..=MAX_SIDE).contains(&side(16)) && (1..=MAX_SIDE).contains(&side(20))
}

/// `<dir>/<host>.png`; hosts are only ever letters, digits, dots, hyphens and colons.
pub fn path(dir: &Path, host: &str) -> PathBuf {
    let name: String = host
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' => c,
            _ => '_',
        })
        .collect();
    dir.join(format!("{name}.png"))
}

/// Like the history file: temporary sibling, then rename.
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("png.tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

/// The letter and palette slot (0..4) for a host without an icon; stable per host.
pub fn monogram(host: &str) -> (char, usize) {
    let host = host.strip_prefix("www.").unwrap_or(host);
    let letter = host
        .chars()
        .find(|c: &char| c.is_alphanumeric())
        .map_or('•', |c| c.to_ascii_uppercase());
    let hash = host.bytes().fold(0u32, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(u32::from(byte))
    });
    (letter, (hash % 4) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 1×1 transparent PNG.
    const PIXEL: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

    #[test]
    fn accepts_a_small_png_and_nothing_else() {
        let png = png_from_data_url(PIXEL).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(png.len(), 70);
        for bad in [
            "data:image/jpeg;base64,AAAA",
            "data:image/png;base64,not*base64",
            "data:image/png;base64,aGVsbG8=",
            "",
        ] {
            assert!(png_from_data_url(bad).is_none(), "{bad}");
        }
        // Oversized dimensions are refused before any decoder runs.
        let mut huge = png_from_data_url(PIXEL).unwrap();
        huge[16..20].copy_from_slice(&20_000u32.to_be_bytes());
        assert!(!valid_png(&huge));
        assert!(png_from_data_url(&format!("{PREFIX}{}", "A".repeat(MAX_DATA_URL))).is_none());
    }

    #[test]
    fn base64_handles_padding_variants() {
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("aGVsbG8").unwrap(), b"hello");
        assert_eq!(base64_decode("aGk=").unwrap(), b"hi");
        assert_eq!(base64_decode("").unwrap(), b"");
    }

    #[test]
    fn paths_and_monograms_are_stable_and_safe() {
        let dir = Path::new("/tmp/icons");
        assert_eq!(
            path(dir, "en.wikipedia.org"),
            dir.join("en.wikipedia.org.png")
        );
        assert_eq!(
            path(dir, "[::1]:8080"),
            dir.join("_::1_:8080.png".replace(':', "_"))
        );
        assert_eq!(monogram("www.rust-lang.org").0, 'R');
        assert_eq!(monogram("127.0.0.1").0, '1');
        assert_eq!(monogram("rust-lang.org"), monogram("www.rust-lang.org"));
        assert!(monogram("example.com").1 < 4);
    }
}
