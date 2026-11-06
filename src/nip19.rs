//! NIP-19's bech32 entities: the bytes behind an `npub`, and the address an `naddr` names.

const CHARSET: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";

/// The human-readable part and the bytes of a bech32 string, if its checksum holds. NIP-19
/// strings run past bech32's 90 characters, so there is no length limit here.
pub fn decode(text: &str) -> Option<(String, Vec<u8>)> {
    if text.chars().any(char::is_uppercase) && text.chars().any(char::is_lowercase) {
        return None;
    }
    let lower = text.to_ascii_lowercase();
    let (human, data) = lower.rsplit_once('1')?;
    let values: Vec<u8> = data
        .bytes()
        .map(|byte| CHARSET.iter().position(|&c| c == byte).map(|at| at as u8))
        .collect::<Option<_>>()?;
    let polymod = |values: &[u8]| {
        const GENERATOR: [u32; 5] = [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3];
        values.iter().fold(1u32, |checksum, &value| {
            let top = checksum >> 25;
            let mut checksum = ((checksum & 0x1ff_ffff) << 5) ^ u32::from(value);
            for (bit, generator) in GENERATOR.iter().enumerate() {
                if (top >> bit) & 1 == 1 {
                    checksum ^= generator;
                }
            }
            checksum
        })
    };
    let mut checked: Vec<u8> = human.bytes().map(|byte| byte >> 5).collect();
    checked.push(0);
    checked.extend(human.bytes().map(|byte| byte & 31));
    checked.extend(&values);
    if values.len() < 6 || polymod(&checked) != 1 {
        return None;
    }
    let payload = &values[..values.len() - 6];
    let (mut accumulator, mut bits, mut bytes) = (0u32, 0u32, Vec::new());
    for &value in payload {
        accumulator = (accumulator << 5) | u32::from(value);
        bits += 5;
        while bits >= 8 {
            bits -= 8;
            bytes.push((accumulator >> bits) as u8);
            accumulator &= (1 << bits) - 1;
        }
    }
    // Padding is zero and shorter than a byte.
    (bits < 5 && accumulator == 0).then(|| (human.to_owned(), bytes))
}

/// What an `naddr` names: an addressable event, and the relays that may hold it.
#[derive(Debug, PartialEq, Eq)]
pub struct Address {
    pub kind: u32,
    /// The author's public key, 64 lowercase hex characters.
    pub author: String,
    pub identifier: String,
    pub relays: Vec<String>,
}

/// The address an `naddr` encodes, if it is one: a TLV of the `d` tag (type 0), relay hints
/// (1), the author (2) and the kind (3).
pub fn naddr(text: &str) -> Option<Address> {
    let (human, bytes) = decode(text)?;
    if human != "naddr" {
        return None;
    }
    let (mut identifier, mut author, mut kind, mut relays) = (None, None, None, Vec::new());
    let mut rest = bytes.as_slice();
    while !rest.is_empty() {
        let (&kind_of, after) = rest.split_first()?;
        let (&length, after) = after.split_first()?;
        let value = after.get(..usize::from(length))?;
        rest = &after[usize::from(length)..];
        match kind_of {
            0 => identifier = Some(String::from_utf8(value.to_vec()).ok()?),
            1 => relays.push(String::from_utf8(value.to_vec()).ok()?),
            2 => author = Some(hex::encode(<[u8; 32]>::try_from(value).ok()?)),
            3 => kind = Some(u32::from_be_bytes(value.try_into().ok()?)),
            _ => {}
        }
    }
    Some(Address {
        kind: kind?,
        author: author?,
        identifier: identifier?,
        relays,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The encoder's side, for the test only.
    fn encode(human: &str, bytes: &[u8]) -> String {
        let mut values = Vec::new();
        let (mut accumulator, mut bits) = (0u32, 0u32);
        for &byte in bytes {
            accumulator = (accumulator << 8) | u32::from(byte);
            bits += 8;
            while bits >= 5 {
                bits -= 5;
                values.push(((accumulator >> bits) & 31) as u8);
            }
        }
        if bits > 0 {
            values.push(((accumulator << (5 - bits)) & 31) as u8);
        }
        let mut checked: Vec<u8> = human.bytes().map(|byte| byte >> 5).collect();
        checked.push(0);
        checked.extend(human.bytes().map(|byte| byte & 31));
        checked.extend(&values);
        checked.extend([0; 6]);
        let polymod = checked.iter().fold(1u32, |checksum, &value| {
            let top = checksum >> 25;
            let mut checksum = ((checksum & 0x1ff_ffff) << 5) ^ u32::from(value);
            for (bit, generator) in [
                0x3b6a57b2u32,
                0x26508e6d,
                0x1ea119fa,
                0x3d4233dd,
                0x2a1462b3,
            ]
            .iter()
            .enumerate()
            {
                if (top >> bit) & 1 == 1 {
                    checksum ^= generator;
                }
            }
            checksum
        }) ^ 1;
        for shift in (0..6).rev() {
            values.push(((polymod >> (5 * shift)) & 31) as u8);
        }
        let data: String = values
            .iter()
            .map(|&value| CHARSET[usize::from(value)] as char)
            .collect();
        format!("{human}1{data}")
    }

    #[test]
    fn an_naddr_names_the_kind_the_author_the_identifier_and_the_relays() {
        let author = [0xabu8; 32];
        let mut tlv = vec![0, 5];
        tlv.extend(b"basic");
        tlv.extend([1, 20]);
        tlv.extend(b"ws://relay.test:7100");
        tlv.extend([2, 32]);
        tlv.extend(author);
        tlv.extend([3, 4]);
        tlv.extend(30432u32.to_be_bytes());
        let text = encode("naddr", &tlv);
        assert_eq!(
            naddr(&text),
            Some(Address {
                kind: 30432,
                author: hex::encode(author),
                identifier: "basic".into(),
                relays: vec!["ws://relay.test:7100".into()],
            })
        );
        assert_eq!(naddr(&encode("npub", &author)), None);
        let broken = format!("{}q", &text[..text.len() - 1]);
        assert_eq!(naddr(&broken), None);
    }
}
