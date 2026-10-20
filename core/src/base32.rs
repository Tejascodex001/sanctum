//! Crockford Base32: digits and letters only, no I, L, O or U.
//!
//! Codes made of these characters are easy to read aloud and type, ignore
//! case, and are encoded by QR in its compact alphanumeric mode. Common
//! typing slips are forgiven: O reads as 0, and I or L as 1.

const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Characters needed for `n` bytes.
pub fn encoded_len(n: usize) -> usize {
    (n * 8).div_ceil(5)
}

pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(encoded_len(bytes.len()));
    let (mut acc, mut bits) = (0u32, 0u32);
    for &b in bytes {
        acc = (acc << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((acc >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((acc << (5 - bits)) & 31) as usize] as char);
    }
    out
}

fn value(c: char) -> Option<u32> {
    let c = match c.to_ascii_uppercase() {
        'O' => '0',
        'I' | 'L' => '1',
        c => c,
    };
    ALPHABET
        .iter()
        .position(|&a| a as char == c)
        .map(|v| v as u32)
}

/// Decodes exactly `n` bytes from the start of `text` (whitespace and
/// hyphens ignored). Returns None if there are too few valid characters.
pub fn decode(text: &str, n: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(n);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.chars().filter(|c| !c.is_whitespace() && *c != '-') {
        if out.len() == n {
            break;
        }
        acc = (acc << 5) | value(c)?;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    (out.len() == n).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_every_length() {
        for len in 0..200 {
            let bytes: Vec<u8> = (0..len).map(|_| rand::random()).collect();
            let text = encode(&bytes);
            assert_eq!(text.len(), encoded_len(len));
            assert!(text.bytes().all(|b| ALPHABET.contains(&b)));
            assert_eq!(decode(&text, len).unwrap(), bytes);
        }
    }

    #[test]
    fn forgiving_input() {
        let bytes = b"sanctum".to_vec();
        let text = encode(&bytes);
        let sloppy = text.to_lowercase().replace('0', "o").replace('1', "l");
        let spaced: String = sloppy.chars().flat_map(|c| [c, ' ']).collect();
        assert_eq!(decode(&spaced, bytes.len()).unwrap(), bytes);
        assert!(decode("U", 1).is_none());
        assert!(decode(&text[..3], bytes.len()).is_none());
    }
}
