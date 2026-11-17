//! Tables de mesures des modules `CAL<n>.DLL`, lues dans le dictionnaire TSV produit sur la machine
//! de l'utilisateur (`scripts/dico_mesures.py`, à partir de sa copie du pack).
//!
//! Colonnes : `module ECUTYID ECUNAME marque fonction requete pos type parametre facteur decalage lecture`.
//! `pos` compte le préfixe de 5 octets de la réponse de la VCI (`01 | adresse | longueur u16 | 05`).
//! Valeur = brut × facteur + décalage.
//!
//! Le type d'une fiche n'a pas de sens universel : chaque fonction de chaque module contient son propre
//! aiguillage. La colonne `lecture` (relevée par `scripts/cal_types.py`) dit ce que la fonction fait d'une
//! fiche de ce type, ex. `get(1) fild16 set_f` = un octet lu, mis à l'échelle, rendu en flottant. Seules
//! les formes reconnues sont décodées ; les autres restent sans valeur.

use crate::{Error, Result};
use std::path::Path;

/// Type de fiche : octet non signé mis à l'échelle (constaté sur la capture du 2026-11-13).
/// Seul type décodé quand le dictionnaire n'a pas de colonne `lecture`.
pub const TYPE_OCTET: u16 = 9;
/// Type de fiche : somme de contrôle, sans valeur.
pub const TYPE_CHECKSUM: u16 = 255;

/// Façon dont la fonction du module lit une fiche.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lecture {
    /// Un octet non signé, puis × facteur + décalage.
    Octet,
    /// Un octet non signé affiché tel quel (facteur et décalage ignorés).
    OctetBrut,
    /// Forme non reconnue (plusieurs octets, chaîne…) : pas de valeur.
    Inconnue,
}

impl Lecture {
    /// Interprète la suite d'appels relevée dans le module ; `None` si la colonne est absente ou vide.
    pub fn depuis(signature: &str) -> Option<Self> {
        let appels: Vec<&str> = signature.split_whitespace().collect();
        if appels.is_empty() {
            return None;
        }
        let nb = |nom: &str| appels.iter().filter(|a| **a == nom).count();
        // lectures de la réponse : `get(n)` (n octets) et `getuc` (un octet)
        let lectures = appels.iter().filter(|a| a.starts_with("get")).count();
        Some(
            if lectures == 1 && nb("get(1)") == 1 && nb("shl") == 0 && nb("fild16") == 1 {
                Lecture::Octet
            } else if lectures == 1 && nb("getuc") == 1 && nb("fild16") == 0 {
                Lecture::OctetBrut
            } else {
                Lecture::Inconnue
            },
        )
    }
}

/// Une mesure dans une trame de réponse.
#[derive(Debug, Clone, PartialEq)]
pub struct Fiche {
    pub nom: String,
    /// Position dans la réponse de la VCI, préfixe de 5 octets compris.
    pub pos: usize,
    pub ty: u16,
    pub facteur: f64,
    pub decalage: f64,
    pub lecture: Lecture,
}

impl Fiche {
    /// Vrai si la valeur de cette fiche sait être calculée.
    pub fn decodable(&self) -> bool {
        self.ty != TYPE_CHECKSUM && self.lecture != Lecture::Inconnue
    }

    /// Valeur physique, si la lecture est reconnue et l'octet présent.
    pub fn valeur(&self, reponse: &[u8]) -> Option<f64> {
        if !self.decodable() {
            return None;
        }
        let brut = f64::from(*reponse.get(self.pos)?);
        Some(match self.lecture {
            Lecture::Octet => brut * self.facteur + self.decalage,
            _ => brut,
        })
    }
}

/// Une fonction de lecture d'un module : requête et fiches de la réponse.
#[derive(Debug, Clone, PartialEq)]
pub struct Trame {
    pub module: String,
    pub ecu_type_id: i64,
    pub fonction: String,
    /// Données de la requête (ex. `c2 80 01`) ; vide si elle n'est pas codée en dur.
    pub requete: Vec<u8>,
    pub fiches: Vec<Fiche>,
}

impl Trame {
    /// (nom, valeur) de chaque fiche, somme de contrôle exclue ; `None` = type non décodé ou réponse trop courte.
    pub fn decode<'a>(&'a self, reponse: &[u8]) -> Vec<(&'a str, Option<f64>)> {
        self.fiches
            .iter()
            .filter(|f| f.ty != TYPE_CHECKSUM)
            .map(|f| (f.nom.as_str(), f.valeur(reponse)))
            .collect()
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Dictionnaire {
    pub trames: Vec<Trame>,
}

fn hex_octets(s: &str) -> Option<Vec<u8>> {
    s.split_whitespace().map(|t| u8::from_str_radix(t, 16).ok()).collect()
}

impl Dictionnaire {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::parse(&String::from_utf8_lossy(&std::fs::read(path)?))
    }

    pub fn parse(tsv: &str) -> Result<Self> {
        let mut trames: Vec<Trame> = Vec::new();
        for (n, l) in tsv.lines().enumerate().skip(1) {
            if l.trim().is_empty() {
                continue;
            }
            let err = |quoi: &str| Error::Format(format!("dictionnaire de mesures, ligne {} : {quoi}", n + 1));
            let c: Vec<&str> = l.split('\t').collect();
            if c.len() < 11 {
                return Err(err("11 colonnes attendues"));
            }
            let ecu_type_id = c[1].parse().map_err(|_| err("ECUTYID"))?;
            let ty: u16 = c[7].parse().map_err(|_| err("type"))?;
            // sans colonne `lecture` (ancien dictionnaire) : seul le type 9, vérifié sur capture, est décodé
            let lecture = match c.get(11) {
                Some(sig) => Lecture::depuis(sig).unwrap_or(Lecture::Inconnue),
                None if ty == TYPE_OCTET => Lecture::Octet,
                None => Lecture::Inconnue,
            };
            let fiche = Fiche {
                nom: c[8].to_string(),
                pos: c[6].parse().map_err(|_| err("pos"))?,
                ty,
                facteur: c[9].parse().map_err(|_| err("facteur"))?,
                decalage: c[10].parse().map_err(|_| err("decalage"))?,
                lecture,
            };
            match trames.last_mut() {
                Some(t) if t.module == c[0] && t.fonction == c[4] => t.fiches.push(fiche),
                _ => trames.push(Trame {
                    module: c[0].to_string(),
                    ecu_type_id,
                    fonction: c[4].to_string(),
                    requete: hex_octets(c[5]).ok_or_else(|| err("requete"))?,
                    fiches: vec![fiche],
                }),
            }
        }
        Ok(Self { trames })
    }

    pub fn trames_of(&self, ecu_type_id: i64) -> impl Iterator<Item = &Trame> {
        self.trames.iter().filter(move |t| t.ecu_type_id == ecu_type_id)
    }

    /// Trame d'un type de calculateur dont la requête est exactement `requete`.
    pub fn trame(&self, ecu_type_id: i64, requete: &[u8]) -> Option<&Trame> {
        self.trames_of(ecu_type_id)
            .find(|t| !t.requete.is_empty() && t.requete == requete)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TSV: &str = "module\tECUTYID\tECUNAME\tmarque\tfonction\trequete\tpos\ttype\tparametre\tfacteur\tdecalage\n\
        CAL9.DLL\t9\tX\tY\tTRAMEA1\ta1 80 01\t7\t9\tA\t32.0\t0.0\n\
        CAL9.DLL\t9\tX\tY\tTRAMEA1\ta1 80 01\t8\t9\tB\t1.0\t-40.0\n\
        CAL9.DLL\t9\tX\tY\tTRAMEA1\ta1 80 01\t9\t4\tC\t1\t0.0\n\
        CAL9.DLL\t9\tX\tY\tTRAMEA1\ta1 80 01\t10\t255\tCHECKSUM\t1\t0.0\n\
        CAL9.DLL\t9\tX\tY\tTRAMEB\t\t3\t9\tD\t1\t0.0\n";

    #[test]
    fn lecture_et_decodage() {
        let d = Dictionnaire::parse(TSV).unwrap();
        assert_eq!(d.trames.len(), 2);
        let t = d.trame(9, &[0xa1, 0x80, 0x01]).unwrap();
        assert_eq!(t.fiches.len(), 4);
        let rep = [0x01, 0x10, 0x05, 0x00, 0x05, 0x61, 0xff, 0x1e, 0x47];
        assert_eq!(t.decode(&rep), vec![("A", Some(960.0)), ("B", Some(31.0)), ("C", None)]);
        assert!(d.trame(9, &[]).is_none());
        assert!(d.trame(8, &[0xa1, 0x80, 0x01]).is_none());
    }

    #[test]
    fn lectures() {
        assert_eq!(
            Lecture::depuis("set_uc set_s get(1) fild16 set_f"),
            Some(Lecture::Octet)
        );
        assert_eq!(Lecture::depuis("get(1) fild16 rt set_uc"), Some(Lecture::Octet));
        assert_eq!(Lecture::depuis("getuc set_s set_i"), Some(Lecture::OctetBrut));
        assert_eq!(
            Lecture::depuis("get(1) fild16 get(1) get(1) shl fild32 set_f"),
            Some(Lecture::Inconnue)
        );
        assert_eq!(
            Lecture::depuis("get(1) shl fild16 get(1) fild16"),
            Some(Lecture::Inconnue)
        );
        assert_eq!(
            Lecture::depuis("getuc set_s set_i getuc rt set_s set_i"),
            Some(Lecture::Inconnue)
        );
        assert_eq!(Lecture::depuis("  "), None);

        let tete =
            "module\tECUTYID\tECUNAME\tmarque\tfonction\trequete\tpos\ttype\tparametre\tfacteur\tdecalage\tlecture\r\n";
        let ligne =
            |ty: u8, nom: &str, sig: &str| format!("CAL9.DLL\t9\tX\tY\tF\ta1\t6\t{ty}\t{nom}\t0.5\t1.0\t{sig}\r\n");
        let tsv = [
            tete.to_string(),
            ligne(4, "A", "get(1) fild16 set_f"),
            ligne(10, "B", "getuc set_s set_i"),
            ligne(4, "C", "get(1) fild16 get(1) fild16 get(1) fild16 set_f"),
            ligne(9, "D", ""),
        ]
        .concat();
        let d = Dictionnaire::parse(&tsv).unwrap();
        let rep = [1, 0x10, 2, 0, 5, 0x61, 10];
        assert_eq!(
            d.trames[0].decode(&rep),
            vec![("A", Some(6.0)), ("B", Some(10.0)), ("C", None), ("D", None)]
        );
    }

    #[test]
    fn erreurs() {
        assert!(Dictionnaire::parse("en-tête\nligne courte\n").is_err());
        assert!(Dictionnaire::parse("en-tête\n").unwrap().trames.is_empty());
    }
}
