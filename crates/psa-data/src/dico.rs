//! Dictionnaires de libellés Lexia `.DBZ` et `.ZCL`.
//!
//! ```text
//! u32 magic        0x13421342 (.DBZ) ou 0x42314231 (.ZCL)
//! u32 header_size  0x64 (DBZ) / 0x6C (ZCL)
//! u32 data_offset  = header_size + 4 * count
//! u32 count
//! ZCL : 8 octets ASCII à 0x64, nom logique « CITTx=0\n »
//! u32 offsets[count] à header_size (offsets absolus)
//! chaînes terminées par NUL, page de code de la langue
//! ```

use crate::text::{decode, Codepage};
use crate::{rd, Error, Result};
use std::path::Path;

const MAGIC_DBZ: u32 = 0x1342_1342;
const MAGIC_ZCL: u32 = 0x4231_4231;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Dbz,
    Zcl,
}

/// Page de code d'une langue Lexia (suffixe du nom de fichier) ; cp1252 par défaut.
pub fn codepage_langue(lang: &str) -> Codepage {
    match lang.to_ascii_uppercase().as_str() {
        "CZ" | "PL" | "HR" | "SL" => Codepage::Cp1250,
        "RU" => Codepage::Cp1251,
        "GR" => Codepage::Cp1253,
        "TR" => Codepage::Cp1254,
        "JA" => Codepage::Cp932,
        "HB" => Codepage::Cp1255,
        _ => Codepage::Cp1252,
    }
}

/// Contrôles de cohérence d'un dictionnaire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Check {
    pub monotone: bool,
    pub first_at_data_offset: bool,
    pub in_bounds: bool,
    pub last_string_ends_at_eof: bool,
    pub empty: usize,
}

pub struct Dico {
    data: Vec<u8>,
    pub kind: Kind,
    pub header_size: u32,
    pub data_offset: u32,
    /// Nom logique (ZCL seulement), ex. `CITTD=0`.
    pub name: Option<String>,
    pub offsets: Vec<u32>,
    pub codepage: Codepage,
}

impl Dico {
    /// Ouvre un dictionnaire ; la page de code vient des 2 dernières lettres du nom.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let n = stem.chars().count();
        let lang: String = stem.chars().skip(n.saturating_sub(2)).collect();
        Self::from_bytes(std::fs::read(path)?, codepage_langue(&lang))
    }

    pub fn from_bytes(data: Vec<u8>, codepage: Codepage) -> Result<Self> {
        let magic = rd::u32(&data, 0, "dico magic")?;
        let header_size = rd::u32(&data, 4, "dico en-tête")?;
        let data_offset = rd::u32(&data, 8, "dico en-tête")?;
        let count = rd::u32(&data, 12, "dico en-tête")?;
        let kind = match magic {
            MAGIC_DBZ => Kind::Dbz,
            MAGIC_ZCL => Kind::Zcl,
            m => return Err(Error::Format(format!("magic de dictionnaire inconnu 0x{m:08x}"))),
        };
        if u64::from(data_offset) != u64::from(header_size) + 4 * u64::from(count) {
            return Err(Error::Format("incohérence data_offset / count".into()));
        }
        let name = (kind == Kind::Zcl).then(|| {
            let n = rd::slice(&data, 0x64, 8);
            let n = n.split(|&c| c == b'\n').next().unwrap_or(&[]);
            n.iter().map(|&c| c as char).collect()
        });
        let hs = header_size as usize;
        let offsets = (0..count as usize)
            .map(|i| rd::u32(&data, hs + 4 * i, "dico offsets"))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            data,
            kind,
            header_size,
            data_offset,
            name,
            offsets,
            codepage,
        })
    }

    pub fn len(&self) -> usize {
        self.offsets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }

    /// Octets de la chaîne `i`, sans le NUL final.
    pub fn raw(&self, i: usize) -> Option<&[u8]> {
        let o = *self.offsets.get(i)? as usize;
        let rest = self.data.get(o..)?;
        let end = rest.iter().position(|&c| c == 0)?;
        Some(&rest[..end])
    }

    /// Libellé `i` décodé.
    pub fn get(&self, i: usize) -> Option<String> {
        self.raw(i).map(|b| decode(b, self.codepage))
    }

    pub fn check(&self) -> Check {
        let o = &self.offsets;
        let last_end = o
            .last()
            .and_then(|_| self.raw(o.len() - 1))
            .map(|r| *o.last().unwrap() as usize + r.len() + 1);
        Check {
            monotone: o.windows(2).all(|w| w[0] <= w[1]),
            first_at_data_offset: o.first() == Some(&self.data_offset),
            in_bounds: o.first() == Some(&self.data_offset)
                && o.last().is_some_and(|&l| (l as usize) < self.data.len()),
            last_string_ends_at_eof: last_end == Some(self.data.len()),
            empty: (0..o.len())
                .filter(|&i| self.raw(i).is_some_and(|r| r.is_empty()))
                .count(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synth(magic: u32, hs: u32, strings: &[&[u8]]) -> Vec<u8> {
        let n = strings.len() as u32;
        let mut d = vec![0u8; hs as usize];
        d[0..4].copy_from_slice(&magic.to_le_bytes());
        d[4..8].copy_from_slice(&hs.to_le_bytes());
        d[8..12].copy_from_slice(&(hs + 4 * n).to_le_bytes());
        d[12..16].copy_from_slice(&n.to_le_bytes());
        if magic == MAGIC_ZCL {
            d[0x64..0x6C].copy_from_slice(b"CITTD=0\n");
        }
        let mut off = hs + 4 * n;
        let mut body = Vec::new();
        for s in strings {
            d.extend_from_slice(&off.to_le_bytes());
            body.extend_from_slice(s);
            body.push(0);
            off += s.len() as u32 + 1;
        }
        d.extend_from_slice(&body);
        d
    }

    #[test]
    fn zcl() {
        let d = Dico::from_bytes(synth(MAGIC_ZCL, 0x6C, &[b"a", b"", b"\xe9t\xe9"]), Codepage::Cp1252).unwrap();
        assert_eq!((d.kind, d.len(), d.name.as_deref()), (Kind::Zcl, 3, Some("CITTD=0")));
        assert_eq!(d.get(2).as_deref(), Some("été"));
        let c = d.check();
        assert!(c.monotone && c.first_at_data_offset && c.in_bounds && c.last_string_ends_at_eof);
        assert_eq!(c.empty, 1);
        assert!(d.get(3).is_none());
    }

    #[test]
    fn dbz_et_erreurs() {
        let d = Dico::from_bytes(synth(MAGIC_DBZ, 0x64, &[b"x"]), Codepage::Cp1252).unwrap();
        assert_eq!((d.kind, d.name.clone()), (Kind::Dbz, None));
        assert!(Dico::from_bytes(vec![0; 32], Codepage::Cp1252).is_err());
        assert!(Dico::from_bytes(vec![1, 2], Codepage::Cp1252).is_err());
    }
}
