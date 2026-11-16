//! Pages de code des fichiers du pack.

/// Page de code d'un texte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codepage {
    Cp1250,
    Cp1251,
    Cp1252,
    Cp1253,
    Cp1254,
    Cp1255,
    Cp932,
    Cp437,
}

const CP1252_80: [char; 32] = [
    '\u{20AC}', '\u{FFFD}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}', '\u{02C6}',
    '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{FFFD}', '\u{017D}', '\u{FFFD}', '\u{FFFD}', '\u{2018}',
    '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}', '\u{02DC}', '\u{2122}', '\u{0161}',
    '\u{203A}', '\u{0153}', '\u{FFFD}', '\u{017E}', '\u{0178}',
];

const CP437_80: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{A0}";

/// Décode `b` ; les octets non définis donnent U+FFFD.
pub fn decode(b: &[u8], cp: Codepage) -> String {
    let enc = match cp {
        Codepage::Cp1252 => {
            return b
                .iter()
                .map(|&c| {
                    if (0x80..0xA0).contains(&c) {
                        CP1252_80[(c - 0x80) as usize]
                    } else {
                        c as char
                    }
                })
                .collect()
        }
        Codepage::Cp437 => {
            let hi: Vec<char> = CP437_80.chars().collect();
            return b
                .iter()
                .map(|&c| if c < 0x80 { c as char } else { hi[(c - 0x80) as usize] })
                .collect();
        }
        Codepage::Cp1250 => encoding_rs::WINDOWS_1250,
        Codepage::Cp1251 => encoding_rs::WINDOWS_1251,
        Codepage::Cp1253 => encoding_rs::WINDOWS_1253,
        Codepage::Cp1254 => encoding_rs::WINDOWS_1254,
        Codepage::Cp1255 => encoding_rs::WINDOWS_1255,
        Codepage::Cp932 => encoding_rs::SHIFT_JIS,
    };
    let s = enc.decode_without_bom_handling(b).0;
    if cp == Codepage::Cp932 {
        return s.into_owned();
    }
    // octets non définis des pages Windows : U+FFFD (encoding_rs les rend en caractères de contrôle C1)
    s.chars()
        .map(|c| {
            if ('\u{80}'..='\u{9F}').contains(&c) {
                '\u{FFFD}'
            } else {
                c
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cp1252() {
        assert_eq!(
            decode(b"instantan\xe9e \x80 \x81", Codepage::Cp1252),
            "instantanée € \u{FFFD}"
        );
    }

    #[test]
    fn cp437() {
        assert_eq!(CP437_80.chars().count(), 128);
        assert_eq!(decode(b"\xb307\xb3 \x82", Codepage::Cp437), "│07│ é");
    }
}
