use super::{Field, Result};
fn jis(row: u8, cell: u8) -> Result<String> {
    if row == 0x2d || (0x79..=0x7c).contains(&row) {
        return Err("Unsupported JIS extension");
    }
    let special = match (row, cell) {
        (0x21, 0x41) => Some("〜"),
        (0x21, 0x42) => Some("‖"),
        (0x21, 0x5d) => Some("−"),
        (0x21, 0x71) => Some("¢"),
        (0x21, 0x72) => Some("£"),
        (0x22, 0x4c) => Some("¬"),
        _ => None,
    };
    if let Some(s) = special {
        return Ok(s.into());
    }
    let bytes = [row | 0x80, cell | 0x80];
    encoding_rs::EUC_JP
        .decode_without_bom_handling_and_without_replacement(&bytes)
        .map(|s| s.into_owned())
        .ok_or("Invalid JIS cell")
}
pub fn notice(bytes: &[u8], trim: bool) -> String {
    let mut out = String::new();
    let mut pos = 0;
    while pos < bytes.len() {
        let a = bytes[pos];
        if pos + 1 < bytes.len() {
            let b = bytes[pos + 1];
            if (0x21..=0x7e).contains(&a)
                && ((0xa1..=0xfe).contains(&b)
                    || ((0x21..=0x7e).contains(&b) && (a >= 0x30 || a == 0x21 && b == 0x21)))
            {
                if let Ok(s) = jis(a, b & 0x7f) {
                    out.push_str(&s);
                    pos += 2;
                    continue;
                }
            }
            if a == 13 && b == 10 {
                out.push('\n');
                pos += 2;
                continue;
            }
        }
        if a == 0 {
            out.push('·');
        } else {
            out.push_str(&format!("<{a:02X}>"));
        }
        pos += 1;
    }
    if trim {
        out.trim_matches('\u{3000}').into()
    } else {
        out
    }
}
// category 0: Japanese, 1: English, 2: separator
fn unit(bytes: &[u8]) -> Result<(String, u8, usize)> {
    let a = *bytes.first().ok_or("Truncated text unit")?;
    if bytes.len() > 1 {
        let b = bytes[1];
        if a == 0x2a && b == 0x6b {
            return Ok((String::new(), 2, 2));
        }
        if a == 0xa9 || a == 0xaa {
            if !(0xa1..=0xfe).contains(&b) {
                return Err("Invalid English cell");
            }
            let c = if a == 0xa9 {
                (b & 127) as char
            } else {
                match b {
                    0xe0 => ' ',
                    0xe5 => 'Ō',
                    0xe8 => 'ū',
                    0xea => 'ō',
                    _ => return Err("Unsupported English cell"),
                }
            };
            return Ok((c.to_string(), 1, 2));
        }
        if a == 0x8e && (0xa1..=0xdf).contains(&b) {
            return Ok((
                char::from_u32(0xff61 + b as u32 - 0xa1)
                    .unwrap()
                    .to_string(),
                0,
                2,
            ));
        }
        if (0xa1..=0xfe).contains(&a) && (0xa1..=0xfe).contains(&b) {
            return Ok((jis(a & 0x7f, b & 0x7f)?, 0, 2));
        }
    }
    if matches!(a, 9 | 10 | 13 | 0x20..=0x7e) {
        Ok(((a as char).to_string(), 0, 1))
    } else {
        Err("Unsupported text byte")
    }
}
pub fn service(bytes: &[u8]) -> Result<Vec<Field>> {
    let mut fields = Vec::new();
    let mut pos = 5;
    for name in [
        "Line",
        "From",
        "To",
        "Direction",
        "Cause",
        "ServiceStatus",
        "Description",
    ] {
        let count = *bytes.get(pos).ok_or("Missing service field")? as usize;
        pos += 1;
        let mut ja = String::new();
        let mut en: Option<String> = None;
        for i in 0..count {
            let (s, cat, consumed) = unit(bytes.get(pos..).ok_or("Truncated service text")?)?;
            pos += consumed;
            match cat {
                2 => {
                    if en.is_some() || i + 1 == count {
                        return Err("Invalid bilingual separator");
                    }
                    en = Some(String::new());
                }
                1 => en.as_mut().ok_or("English without separator")?.push_str(&s),
                _ => {
                    if en.is_some() {
                        return Err("Invalid bilingual order");
                    }
                    ja.push_str(&s);
                }
            }
        }
        fields.push(Field {
            field: name.into(),
            value: ja,
            english: en,
        });
    }
    if pos != bytes.len() {
        return Err("Trailing service text");
    }
    Ok(fields)
}
