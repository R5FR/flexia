//! Tables dBase III (`.DBF`) de la documentation de Lexia, en lecture seule.
//!
//! Lexia y range le libellé des codes méthode produits par les calculateurs dont le module `CAL<n>` lit une
//! zone en champs de bits (combiné, climatisation, afficheur…) : `SGML/Menu/MENU904H.DBF` a les colonnes
//! `DEFAUT` (ex. `H7X`) et `PHRASE` (libellé brut, ex. `@P10069-thesau@\n@P5424-CITACT@\+@P3406-CITACT`),
//! constaté le 2026-11-17 ; la table d'un calculateur est désignée par l'article « Lecture défauts » de ses
//! menus (voir [`crate::defauts`]).
//!
//! Format : nombre d'enregistrements (u32 à +4), taille de l'en-tête (u16 à +8), taille d'un enregistrement
//! (u16 à +10), descripteurs de champ de 32 octets à partir de +32 (nom sur 11 octets, longueur à +16) jusqu'à
//! l'octet `0x0D` ; chaque enregistrement commence par un octet d'effacement (`*` = effacé).

use crate::rd;
use crate::text::{decode, Codepage};
use crate::{Error, Result};
use std::collections::BTreeMap;
use std::path::Path;

/// Contenu d'une table : noms des champs et lignes non effacées (valeurs sans les espaces de fin).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub champs: Vec<String>,
    pub lignes: Vec<Vec<String>>,
}

impl Table {
    pub fn ouvrir(chemin: impl AsRef<Path>) -> Result<Self> {
        Self::lire(&std::fs::read(chemin)?)
    }

    pub fn lire(b: &[u8]) -> Result<Self> {
        let n = rd::u32(b, 4, "DBF : nombre d'enregistrements")? as usize;
        let entete = rd::u16(b, 8, "DBF : taille de l'en-tête")? as usize;
        let taille = rd::u16(b, 10, "DBF : taille d'un enregistrement")? as usize;
        let mut champs = Vec::new();
        let mut o = 32;
        while rd::u8(b, o, "DBF : descripteurs de champ")? != 0x0D {
            if o + 32 > entete {
                return Err(Error::Format("DBF : descripteurs de champ sans fin".into()));
            }
            let nom = rd::slice(b, o, 11).split(|&c| c == 0).next().unwrap_or_default();
            let longueur = rd::u8(b, o + 16, "DBF : longueur de champ")? as usize;
            champs.push((String::from_utf8_lossy(nom).into_owned(), longueur));
            o += 32;
        }
        if taille < 1 + champs.iter().map(|(_, l)| l).sum::<usize>() {
            return Err(Error::Format("DBF : enregistrement plus court que ses champs".into()));
        }
        let mut lignes = Vec::with_capacity(n);
        for k in 0..n {
            let debut = entete + k * taille;
            let r = b.get(debut..debut + taille).ok_or(Error::Truncated {
                what: "DBF : enregistrement",
                offset: debut,
            })?;
            if r[0] == b'*' {
                continue;
            }
            let mut p = 1;
            let mut ligne = Vec::with_capacity(champs.len());
            for (_, l) in &champs {
                ligne.push(decode(&r[p..p + l], Codepage::Cp1252).trim_end().to_string());
                p += l;
            }
            lignes.push(ligne);
        }
        Ok(Self {
            champs: champs.into_iter().map(|(nom, _)| nom).collect(),
            lignes,
        })
    }

    /// Rang d'un champ, casse ignorée.
    pub fn colonne(&self, nom: &str) -> Option<usize> {
        self.champs.iter().position(|c| c.eq_ignore_ascii_case(nom))
    }

    /// (clé -> valeur) de deux colonnes ; la première ligne d'une clé l'emporte, les clés vides sont écartées.
    pub fn paires(&self, cle: &str, valeur: &str) -> Option<BTreeMap<String, String>> {
        let (c, v) = (self.colonne(cle)?, self.colonne(valeur)?);
        let mut out = BTreeMap::new();
        for l in &self.lignes {
            if !l[c].is_empty() {
                out.entry(l[c].clone()).or_insert_with(|| l[v].clone());
            }
        }
        Some(out)
    }
}

/// Libellés bruts d'une table de défauts de menu : code méthode (`DEFAUT`) -> libellé (`PHRASE`).
pub fn libelles_defauts(chemin: impl AsRef<Path>) -> Result<BTreeMap<String, String>> {
    let p = chemin.as_ref();
    Table::ouvrir(p)?
        .paires("DEFAUT", "PHRASE")
        .ok_or_else(|| Error::Format(format!("{} : colonnes DEFAUT / PHRASE absentes", p.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Petite table construite à la main : deux champs, trois enregistrements dont un effacé.
    fn table() -> Vec<u8> {
        let champs = [("DEFAUT", 10u8), ("PHRASE", 20u8)];
        let taille = 1 + 10 + 20;
        let entete = 32 + 32 * champs.len() + 1;
        let mut b = vec![0u8; 32];
        b[0] = 0x03;
        b[4..8].copy_from_slice(&3u32.to_le_bytes());
        b[8..10].copy_from_slice(&(entete as u16).to_le_bytes());
        b[10..12].copy_from_slice(&(taille as u16).to_le_bytes());
        for (nom, l) in champs {
            let mut d = [0u8; 32];
            d[..nom.len()].copy_from_slice(nom.as_bytes());
            d[11] = b'C';
            d[16] = l;
            b.extend_from_slice(&d);
        }
        b.push(0x0D);
        for (eff, code, phrase) in [
            (b' ', "00Z", "@P11833-thesau"),
            (b'*', "H7X", "efface"),
            (b' ', "H7X", "@P1-X"),
        ] {
            b.push(eff);
            b.extend_from_slice(format!("{code:<10}{phrase:<20}").as_bytes());
        }
        b.push(0x1A);
        b
    }

    #[test]
    fn lecture() {
        let t = Table::lire(&table()).unwrap();
        assert_eq!(t.champs, ["DEFAUT", "PHRASE"]);
        assert_eq!(t.lignes.len(), 2, "l'enregistrement effacé est écarté");
        let p = t.paires("defaut", "phrase").unwrap();
        assert_eq!(p.get("H7X").map(String::as_str), Some("@P1-X"));
        assert_eq!(p.get("00Z").map(String::as_str), Some("@P11833-thesau"));
        assert!(t.paires("DEFAUT", "ABSENT").is_none());
    }

    #[test]
    fn tronquee() {
        let b = table();
        assert!(Table::lire(&b[..b.len() - 12]).is_err());
        assert!(Table::lire(&b[..20]).is_err());
    }
}
