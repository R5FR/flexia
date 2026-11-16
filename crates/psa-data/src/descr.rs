//! Descripteurs et tables de commandes de la VCI ACTIA.
//!
//! - `SUPER*.PC` : table CP437 « │CL│CD│ TEMPO│ LIBELLE │ » ;
//! - `P_*.68K` / `C_*.68K` : descripteurs ASCII, enregistrements fixes de 45 caractères ;
//! - `P_*.pro` : descripteurs binaires, enregistrements « lettre, index, valeur ».

use crate::text::{decode, Codepage};
use crate::{rd, Result};
use std::collections::BTreeMap;

/// Ligne d'un `SUPER*.PC` : commande système de la VCI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PcRow {
    pub classe: u8,
    pub code: u8,
    /// Délai en ms.
    pub delai: u32,
    pub libelle: String,
}

fn hex2(s: &str) -> Option<u8> {
    (s.len() == 2 && s.bytes().all(|c| c.is_ascii_hexdigit()))
        .then(|| u8::from_str_radix(s, 16).ok())?
}

pub fn parse_pc(data: &[u8]) -> Vec<PcRow> {
    let t = decode(data, Codepage::Cp437);
    let mut rows = Vec::new();
    for line in t.split(['\n', '\r']) {
        let p: Vec<&str> = line.split('│').collect();
        if p.len() < 6 || !p[0].chars().all(char::is_whitespace) {
            continue;
        }
        let delai = p[3].trim_start();
        if delai.is_empty() || !delai.bytes().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if let (Some(classe), Some(code), Ok(delai)) = (hex2(p[1]), hex2(p[2]), delai.parse()) {
            rows.push(PcRow {
                classe,
                code,
                delai,
                libelle: p[4].trim().to_string(),
            });
        }
    }
    rows
}

/// Longueur d'un enregistrement `.68K`.
pub const REC68K: usize = 45;

/// Paramètres de ligne série d'un enregistrement `.68K` (ex. `181N10400`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ligne {
    pub start: u8,
    pub data: u8,
    pub stop: u8,
    pub parite: char,
    pub bauds: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rec68k {
    pub raw: String,
    pub id: String,
    pub ligne: Option<Ligne>,
}

fn cherche_ligne(r: &[char]) -> Option<Ligne> {
    let dg = |c: char| c.to_digit(10).filter(|_| c.is_ascii_digit());
    for i in 0..r.len().saturating_sub(3) {
        let (Some(a), Some(b), Some(c)) = (dg(r[i]), dg(r[i + 1]), dg(r[i + 2])) else {
            continue;
        };
        if !matches!(r[i + 3], 'N' | 'E' | 'O') {
            continue;
        }
        let mut j = i + 4;
        while j < r.len() && r[j].is_whitespace() {
            j += 1;
        }
        let n = r[j..]
            .iter()
            .take(6)
            .take_while(|c| c.is_ascii_digit())
            .count();
        if n >= 3 {
            let bauds = r[j..j + n].iter().collect::<String>().parse().ok()?;
            return Some(Ligne {
                start: a as u8,
                data: b as u8,
                stop: c as u8,
                parite: r[i + 3],
                bauds,
            });
        }
    }
    None
}

pub fn parse_68k(data: &[u8]) -> Vec<Rec68k> {
    let s: Vec<char> = data
        .iter()
        .filter(|&&c| c != b'\r' && c != b'\n')
        .map(|&c| if c < 0x80 { c as char } else { '\u{FFFD}' })
        .collect();
    s.chunks(REC68K)
        .filter(|r| r.iter().any(|&c| c != '-'))
        .map(|r| Rec68k {
            raw: r.iter().collect(),
            id: r
                .iter()
                .take(6)
                .collect::<String>()
                .trim_matches('-')
                .to_string(),
            ligne: cherche_ligne(r),
        })
        .collect()
}

/// Enregistrements d'un Code Com dans un `P_*.68K`.
///
/// `H` = octet de format KWP, `T` = cible, `S` = source, `D` = données (le premier est le service),
/// `r` = une réponse est attendue.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cmd68k {
    pub h: Vec<String>,
    pub t: Vec<String>,
    pub s: Vec<String>,
    pub d: Vec<String>,
    pub r: bool,
}

/// Commandes par Code Com (2 caractères hexadécimaux) et lignes `P`/`C` (temporisations, init).
///
/// Enregistrement : `[0]` type, `[1:3]` Code cal, `[3:5]` Code Com, `[16:25]` valeur.
pub fn commandes_68k(recs: &[Rec68k]) -> (BTreeMap<String, Cmd68k>, Vec<String>) {
    let mut cmds: BTreeMap<String, Cmd68k> = BTreeMap::new();
    let mut divers = Vec::new();
    for d in recs {
        let r: Vec<char> = d.raw.chars().collect();
        let sub = |a: usize, b: usize| -> String { r.iter().skip(a).take(b - a).collect() };
        let (t, com) = (r[0], sub(3, 5));
        let val = sub(16, 25).trim_end_matches('-').to_string();
        if t == 'P' || t == 'C' {
            divers.push(d.raw.trim_end_matches('-').to_string());
        } else if "HTSD-".contains(t) && !com.trim_matches('-').is_empty() {
            let c = cmds.entry(com).or_default();
            match t {
                'H' => c.h.push(val),
                'T' => c.t.push(val),
                'S' => c.s.push(val),
                'D' => c.d.push(val),
                _ => c.r = d.raw.contains('R'),
            }
        }
    }
    (cmds, divers)
}

/// Enregistrement d'un `P_*.pro`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProRec {
    /// Bloc à 2 lettres (`CU`, `CP`…) : deux `u16`.
    Bloc { tag: String, a: u16, b: u16 },
    /// `D` / `A` : lettre + `u8`.
    Court { tag: char, valeur: u8 },
    /// Lettre, index, valeur (`Q` : 32 bits, sinon 16 bits).
    Val { tag: char, index: u8, valeur: u32 },
    /// Non reconnu : 4 octets à `offset`.
    Inconnu { offset: usize, octets: Vec<u8> },
}

pub fn parse_pro(b: &[u8]) -> Result<Vec<ProRec>> {
    let mut i = 0;
    let mut recs = Vec::new();
    while i + 4 <= b.len() {
        let tag = b[i] as char;
        if b[i] == b'C' && b[i + 1].is_ascii_uppercase() {
            let (a, c) = (
                rd::u16(b, i + 2, ".pro bloc")?,
                rd::u16(b, i + 4, ".pro bloc")?,
            );
            recs.push(ProRec::Bloc {
                tag: format!("C{}", b[i + 1] as char),
                a,
                b: c,
            });
            i += 6;
        } else if tag == 'D' || tag == 'A' {
            recs.push(ProRec::Court {
                tag,
                valeur: b[i + 1],
            });
            i += 2;
        } else if tag == 'Q' {
            let (lo, hi) = (rd::u16(b, i + 2, ".pro Q")?, rd::u16(b, i + 4, ".pro Q")?);
            recs.push(ProRec::Val {
                tag,
                index: b[i + 1],
                valeur: u32::from(hi) << 16 | u32::from(lo),
            });
            i += 6;
        } else if tag.is_alphabetic() {
            recs.push(ProRec::Val {
                tag,
                index: b[i + 1],
                valeur: u32::from(rd::u16(b, i + 2, ".pro")?),
            });
            i += 4;
        } else {
            recs.push(ProRec::Inconnu {
                offset: i,
                octets: b[i..i + 4].to_vec(),
            });
            i += 4;
        }
    }
    Ok(recs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pc() {
        let mut l = Vec::new();
        l.extend_from_slice(b" \xb301\xb30A\xb3  4000\xb3 BOOT - ECHO      \xb3\r\n");
        l.extend_from_slice(b"\xb3CL\xb3CD\xb3 TEMPO\xb3 LIBELLE \xb3\r\n");
        let r = parse_pc(&l);
        assert_eq!(
            r,
            vec![PcRow {
                classe: 1,
                code: 10,
                delai: 4000,
                libelle: "BOOT - ECHO".into()
            }]
        );
    }

    fn rec(s: &str) -> String {
        format!("{s:-<45}")
    }

    #[test]
    fn k68() {
        let txt = [
            rec("H8E07-----------E81"),
            rec("T8E07-----------E10"),
            rec("D8E07-----------E21"),
            rec("D8E07-----------E--1  1+"),
            rec("-8E07-----------R"),
            rec("P8E-------------181N10400"),
            rec(""),
        ]
        .join("\r\n");
        let recs = parse_68k(txt.as_bytes());
        assert_eq!(recs.len(), 6);
        assert_eq!(
            recs[5].ligne,
            Some(Ligne {
                start: 1,
                data: 8,
                stop: 1,
                parite: 'N',
                bauds: 10400
            })
        );
        let (cmds, divers) = commandes_68k(&recs);
        let c = &cmds["07"];
        assert_eq!(
            (c.h.as_slice(), c.t.as_slice()),
            (&["E81".to_string()][..], &["E10".to_string()][..])
        );
        assert_eq!(c.d, vec!["E21", "E--1  1+"]);
        assert!(c.r);
        assert_eq!(divers, vec!["P8E-------------181N10400"]);
    }

    #[test]
    fn pro() {
        let b = [
            b'C', b'U', 1, 0, 2, 0, b'D', 7, b'Q', 1, 0xA0, 0x06, 0xDA, 0x18, b'P', 1, 0xA7, 0x06,
            0, 0, 0, 0,
        ];
        let r = parse_pro(&b).unwrap();
        assert_eq!(
            r[0],
            ProRec::Bloc {
                tag: "CU".into(),
                a: 1,
                b: 2
            }
        );
        assert_eq!(
            r[1],
            ProRec::Court {
                tag: 'D',
                valeur: 7
            }
        );
        assert_eq!(
            r[2],
            ProRec::Val {
                tag: 'Q',
                index: 1,
                valeur: 0x18DA06A0
            }
        );
        assert_eq!(
            r[3],
            ProRec::Val {
                tag: 'P',
                index: 1,
                valeur: 0x6A7
            }
        );
        assert!(matches!(r[4], ProRec::Inconnu { offset: 18, .. }));
    }
}
