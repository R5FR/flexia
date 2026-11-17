//! Défauts en champs de bits : calculateurs dont le module `CAL<n>.DLL` lit une zone (ex. `21 c3`) et en tire
//! des codes méthode PSA (combiné, climatisation, afficheur…), au lieu de codes KWP (service `17`).
//!
//! Les tables viennent du dictionnaire TSV produit sur la machine de l'utilisateur, à partir de sa copie du
//! pack (`scripts/cal_defauts.py`, qui exécute en émulation la fonction `LECTDEF` de chaque module, bit par
//! bit). Une ligne par élément :
//!
//! ```text
//! nature  ecutyid  ecuname  vehicule  fonction  commande  donnees  octet  masque  code
//! bit       246    COMBINE  *         LECTDEF4014  LECTURE_DEFAUTS  ""  6  80  H7X
//! aucun     246    COMBINE  *         LECTDEF4014  LECTURE_DEFAUTS  ""  ""  ""  00Z
//! libelles  246    COMBINE  CITROENC5 MENU900W  ""  ""  ""  ""  MENU904H
//! ```
//!
//! - `bit` : le bit `masque` de l'octet `octet` de la réponse KWP (`61` = octet 0) donne `code` ;
//! - `aucun` : code rendu sans aucun bit à 1 (`00Z`, « absence de défaut ») ;
//! - `libelles` : table `SGML/Menu/<code>.DBF` des libellés de ce calculateur sur ce véhicule ;
//! - `vehicule` = `*` quand le module ne tient pas compte du véhicule ; `donnees` = octets que le module ajoute
//!   à la commande (en hexadécimal).
//!
//! Constaté sur la capture du 2026-11-13 : combiné `61 c3 00 00 00 00 e0 00` -> H7X, H8X, H9X ; afficheur
//! `61 83 40 …` -> 0GZ ; climatisation `61 c4 00…00 80` -> aucun code (son dernier octet n'est pas lu).

use crate::{Error, Result};
use std::collections::BTreeMap;
use std::path::Path;

/// Table d'un type de calculateur (sur un véhicule, ou `*`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableBits {
    pub ecu_type_id: i64,
    pub vehicule: String,
    /// Fonction du module qui a donné la table, ex. `LECTDEF4014`.
    pub fonction: String,
    /// Commande nommée envoyée, ex. `LECTURE_DEFAUTS`.
    pub commande: String,
    /// Octets ajoutés par le module aux données de la base (ex. `83` pour `LEC_TRM` sur l'afficheur).
    pub donnees: Vec<u8>,
    /// Code rendu quand aucun bit n'est à 1.
    pub aucun: String,
    /// (octet de la réponse KWP, masque, code), dans l'ordre du dictionnaire.
    pub bits: Vec<(usize, u8, String)>,
}

/// Vrai pour un code fugitif (`H…`, `I…` : défaut apparu puis disparu), faux pour un code permanent.
pub fn fugitif(code: &str) -> bool {
    code.chars().next().is_some_and(|c| ('H'..='Q').contains(&c))
}

/// Code permanent d'un code fugitif : `H7X` -> `07X`, `IDZ` -> `1DZ`.
fn permanent_de(code: &str) -> Option<String> {
    let c = code.chars().next().filter(|c| ('H'..='Q').contains(c))?;
    let chiffre = char::from(b'0' + (c as u8 - b'H'));
    Some(format!("{chiffre}{}", &code[1..]))
}

impl TableBits {
    /// Codes produits par une réponse KWP, comme le module : un code par bit à 1, sans doublon, dans l'ordre de
    /// la table ; un code fugitif est omis quand son code permanent est aussi produit (le module teste le bit
    /// « permanent » d'abord : constaté dans CAL246 et CAL270). Aucun bit à 1 : liste vide.
    pub fn decoder(&self, kwp: &[u8]) -> Vec<String> {
        let mut codes: Vec<String> = Vec::new();
        for (octet, masque, code) in &self.bits {
            if kwp.get(*octet).is_some_and(|b| b & masque != 0) && !codes.contains(code) {
                codes.push(code.clone());
            }
        }
        let permanents: Vec<String> = codes.iter().filter(|c| !fugitif(c)).cloned().collect();
        codes.retain(|c| permanent_de(c).is_none_or(|p| !permanents.contains(&p)));
        codes
    }

    fn meme_lecture(&self, autre: &TableBits) -> bool {
        self.commande == autre.commande && self.donnees == autre.donnees && self.bits == autre.bits
    }
}

/// Dictionnaire complet : tables de bits et tables de libellés.
#[derive(Debug, Default)]
pub struct DictionnaireDefauts {
    tables: Vec<TableBits>,
    /// (type de calculateur, véhicule) -> nom de la table `SGML/Menu/<nom>.DBF`.
    libelles: BTreeMap<(i64, String), String>,
}

impl DictionnaireDefauts {
    pub fn charger(chemin: impl AsRef<Path>) -> Result<Self> {
        Self::lire(&std::fs::read_to_string(chemin)?)
    }

    pub fn lire(texte: &str) -> Result<Self> {
        let mut lignes = texte.lines();
        let entete: Vec<&str> = lignes.next().unwrap_or("").split('\t').collect();
        let attendu = [
            "nature", "ecutyid", "ecuname", "vehicule", "fonction", "commande", "donnees", "octet", "masque", "code",
        ];
        if entete != attendu {
            return Err(Error::Format(format!("dictionnaire des défauts : en-tête {entete:?}")));
        }
        let mut d = Self::default();
        for (n, l) in lignes.enumerate() {
            let c: Vec<&str> = l.split('\t').collect();
            let err = |quoi: &str| Error::Format(format!("dictionnaire des défauts, ligne {} : {quoi}", n + 2));
            if c.len() != attendu.len() {
                return Err(err("nombre de colonnes"));
            }
            let id: i64 = c[1].parse().map_err(|_| err("ecutyid"))?;
            match c[0] {
                "libelles" => {
                    d.libelles.insert((id, c[3].to_string()), c[9].to_string());
                }
                "aucun" | "bit" => {
                    let donnees = (0..c[6].len())
                        .step_by(2)
                        .map(|i| c[6].get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()))
                        .collect::<Option<Vec<u8>>>()
                        .ok_or_else(|| err("donnees"))?;
                    let pos = d.tables.iter().position(|t| {
                        t.ecu_type_id == id && t.vehicule == c[3] && t.fonction == c[4] && t.commande == c[5]
                    });
                    let i = pos.unwrap_or_else(|| {
                        d.tables.push(TableBits {
                            ecu_type_id: id,
                            vehicule: c[3].to_string(),
                            fonction: c[4].to_string(),
                            commande: c[5].to_string(),
                            donnees,
                            aucun: String::new(),
                            bits: Vec::new(),
                        });
                        d.tables.len() - 1
                    });
                    if c[0] == "aucun" {
                        d.tables[i].aucun = c[9].to_string();
                    } else {
                        let octet = c[7].parse().map_err(|_| err("octet"))?;
                        let masque = u8::from_str_radix(c[8], 16).map_err(|_| err("masque"))?;
                        d.tables[i].bits.push((octet, masque, c[9].to_string()));
                    }
                }
                autre => return Err(err(&format!("nature « {autre} »"))),
            }
        }
        Ok(d)
    }

    /// Nombre de tables de bits.
    pub fn len(&self) -> usize {
        self.tables.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tables.is_empty()
    }

    /// Table d'un type de calculateur : celle du véhicule donné, sinon celle valable pour tous (`*`) ; sans
    /// véhicule, la table commune ou, à défaut, la seule table du type. Quand un module a plusieurs fonctions
    /// `LECTDEF`, celle dont le nom ne contient pas `ECU` est préférée.
    pub fn table(&self, ecu_type_id: i64, vehicule: Option<&str>) -> Option<&TableBits> {
        let du_type: Vec<&TableBits> = self.tables.iter().filter(|t| t.ecu_type_id == ecu_type_id).collect();
        let choisir = |v: &str| {
            let c: Vec<&&TableBits> = du_type.iter().filter(|t| t.vehicule == v).collect();
            c.iter()
                .find(|t| !t.fonction.contains("ECU"))
                .or(c.first())
                .map(|t| **t)
        };
        match vehicule {
            Some(v) => choisir(v).or_else(|| choisir("*")),
            None => choisir("*").or_else(|| {
                let mut v: Vec<&str> = du_type.iter().map(|t| t.vehicule.as_str()).collect();
                v.dedup();
                (v.len() == 1).then(|| choisir(v[0])).flatten()
            }),
        }
    }

    /// Table de libellés (`SGML/Menu/<nom>.DBF`) d'un type de calculateur sur un véhicule. À défaut, celle
    /// d'un autre type du même véhicule dont la table de bits est identique (cas des afficheurs A/C de la C5,
    /// dont les menus ne mènent pas à une table : supposé, les quatre afficheurs ont la même lecture).
    pub fn table_libelles(&self, ecu_type_id: i64, vehicule: &str) -> Option<&str> {
        if let Some(t) = self.libelles.get(&(ecu_type_id, vehicule.to_string())) {
            return Some(t);
        }
        let la_mienne = self.table(ecu_type_id, Some(vehicule))?;
        self.libelles
            .iter()
            .filter(|((id, v), _)| v == vehicule && *id != ecu_type_id)
            .find(|((id, _), _)| {
                self.table(*id, Some(vehicule))
                    .is_some_and(|t| t.meme_lecture(la_mienne))
            })
            .map(|(_, nom)| nom.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TSV: &str = "nature\tecutyid\tecuname\tvehicule\tfonction\tcommande\tdonnees\toctet\tmasque\tcode
aucun\t246\tCOMBINE\t*\tLECTDEF4014\tLECTURE_DEFAUTS\t\t\t\t00Z
bit\t246\tCOMBINE\t*\tLECTDEF4014\tLECTURE_DEFAUTS\t\t2\t02\t06Z
bit\t246\tCOMBINE\t*\tLECTDEF4014\tLECTURE_DEFAUTS\t\t2\t04\t06Z
bit\t246\tCOMBINE\t*\tLECTDEF4014\tLECTURE_DEFAUTS\t\t3\t80\t07X
bit\t246\tCOMBINE\t*\tLECTDEF4014\tLECTURE_DEFAUTS\t\t6\t80\tH7X
bit\t246\tCOMBINE\t*\tLECTDEF4014\tLECTURE_DEFAUTS\t\t6\t40\tH8X
bit\t246\tCOMBINE\t*\tLECTDEF4014\tLECTURE_DEFAUTS\t\t6\t20\tH9X
libelles\t246\tCOMBINE\tCITROENC5\tMENU900W\t\t\t\t\tMENU904H
aucun\t319\tECRAN_B\tCITROENC5\tLECTDEF4759\tLEC_TRM\t83\t\t\t00Z
bit\t319\tECRAN_B\tCITROENC5\tLECTDEF4759\tLEC_TRM\t83\t2\t40\t0GZ
aucun\t255\tECRAN_A\tCITROENC5\tLECTDEF4759\tLEC_TRM\t83\t\t\t00Z
bit\t255\tECRAN_A\tCITROENC5\tLECTDEF4759\tLEC_TRM\t83\t2\t40\t0GZ
aucun\t255\tECRAN_A\tN7\tLECTDEF4759\tLEC_TRM\t83\t\t\t00Z
libelles\t319\tECRAN_B\tCITROENC5\tMENUA005\t\t\t\t\tMENU906V
";

    #[test]
    fn combine_du_13_11() {
        let d = DictionnaireDefauts::lire(TSV).unwrap();
        assert_eq!(d.len(), 4);
        let t = d.table(246, Some("CITROENC5")).unwrap();
        assert_eq!(
            (t.vehicule.as_str(), t.commande.as_str(), t.aucun.as_str()),
            ("*", "LECTURE_DEFAUTS", "00Z")
        );
        let kwp = [0x61, 0xc3, 0x00, 0x00, 0x00, 0x00, 0xe0, 0x00];
        assert_eq!(t.decoder(&kwp), ["H7X", "H8X", "H9X"]);
        assert!(t.decoder(&[0x61, 0xc3, 0, 0, 0, 0, 0, 0]).is_empty());
        // deux bits du même code : un seul code ; permanent et fugitif du même défaut : le permanent seul
        assert_eq!(t.decoder(&[0x61, 0xc3, 0x06, 0x80, 0, 0, 0x80, 0]), ["06Z", "07X"]);
        assert_eq!(d.table_libelles(246, "CITROENC5"), Some("MENU904H"));
        assert_eq!(d.table_libelles(246, "N7"), None);
    }

    #[test]
    fn vehicule_et_repli() {
        let d = DictionnaireDefauts::lire(TSV).unwrap();
        let t = d.table(319, Some("CITROENC5")).unwrap();
        assert_eq!((t.commande.as_str(), t.donnees.as_slice()), ("LEC_TRM", &[0x83][..]));
        assert_eq!(t.decoder(&[0x61, 0x83, 0x40, 0, 0]), ["0GZ"]);
        assert!(d.table(319, Some("N7")).is_none());
        assert!(
            d.table(255, None).is_none(),
            "deux véhicules, aucun commun : pas de choix"
        );
        assert_eq!(d.table(319, None).map(|t| t.vehicule.as_str()), Some("CITROENC5"));
        // l'afficheur A n'a pas de table de libellés : celle de l'afficheur B, même lecture
        assert_eq!(d.table_libelles(255, "CITROENC5"), Some("MENU906V"));
        assert!(fugitif("H7X") && fugitif("IDZ") && !fugitif("0GZ") && !fugitif("1DZ"));
        assert_eq!(permanent_de("IDZ").as_deref(), Some("1DZ"));
    }

    #[test]
    fn erreurs() {
        assert!(DictionnaireDefauts::lire("a\tb\n").is_err());
        let mauvais = TSV.replace("\t6\t80\tH7X", "\tsix\t80\tH7X");
        assert!(DictionnaireDefauts::lire(&mauvais).is_err());
    }
}
