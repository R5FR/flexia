//! Garde « lecture seule » : tout ce qui n'est pas explicitement autorisé est refusé.
//!
//! Il n'existe aucune option pour la désactiver. Une opération d'écriture ne pourra être ajoutée
//! qu'une par une, nommément (plan §9).

use psa_data::descr::Cmd68k;
use std::fmt;

/// Commandes système de la VCI autorisées : celles vues dans la session du 2026-11-13.
/// `07` retour au boot, `09` lecture EEPROM, `0b` voltmètre, `12`/`fa` version, `fe` statut,
/// `04`/`05` chargement des descripteurs `P_`/`C_*.68K`. Téléchargement et flash du firmware : refusés.
pub const SYSTEME_AUTORISE: [u8; 8] = [0x04, 0x05, 0x07, 0x09, 0x0b, 0x12, 0xfa, 0xfe];

/// Services KWP2000 autorisés : StartCommunication, StopCommunication, TesterPresent,
/// ReadStatusOfDTC, ReadDTCByStatus, ReadDataByLocalIdentifier, ReadECUIdentification.
pub const SERVICES_AUTORISES: [u8; 7] = [0x81, 0x82, 0x3E, 0x17, 0x18, 0x21, 0x1A];

/// Noms de commande refusés quel que soit leur service.
pub const NOMS_REFUSES: [&str; 14] = [
    "TELECODAGE",
    "PILOTAGE",
    "EFFACE_DEFAUTS",
    "EFFACEMENT",
    "RAZ_CHGT_ETATS",
    "RESET_ECU",
    "REQDWN",
    "TELECHARGEMENT",
    "DEMARRE_ROUTINE",
    "ARRET_ROUTINE",
    "ACTIVATION",
    "SECURITY_ACCESS_1",
    "SECURITY_ACCESS_2",
    "MAPPING",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refus {
    Systeme(u8),
    Nom(String),
    /// Service KWP non autorisé.
    Service(u8),
    /// Le service ne peut pas être déterminé (descripteur absent ou non compris).
    ServiceInconnu(String),
}

impl fmt::Display for Refus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refus::Systeme(c) => write!(f, "commande système {c:02x} non autorisée"),
            Refus::Nom(n) => write!(f, "commande « {n} » refusée (lecture seule)"),
            Refus::Service(s) => write!(f, "service KWP {s:02x} non autorisé (lecture seule)"),
            Refus::ServiceInconnu(m) => write!(f, "service KWP indéterminé : {m}"),
        }
    }
}

pub fn systeme(code_com: u8) -> Result<(), Refus> {
    SYSTEME_AUTORISE
        .contains(&code_com)
        .then_some(())
        .ok_or(Refus::Systeme(code_com))
}

/// Service KWP qu'enverra la VCI pour ce Code Com : 1er enregistrement `D` du `.68K`.
/// `E<hh>` = octet fixe ; `E--1  1+` = octets repris des données de la requête à partir du premier.
pub fn service(desc: Option<&Cmd68k>, data: &[u8]) -> Result<u8, Refus> {
    let d = desc
        .and_then(|c| c.d.first())
        .ok_or_else(|| Refus::ServiceInconnu("pas d'enregistrement D pour ce Code Com".into()))?;
    let inconnu = || Refus::ServiceInconnu(format!("enregistrement D « {d} »"));
    let reste = d.strip_prefix('E').ok_or_else(inconnu)?;
    if reste.starts_with("--") {
        // Seule la forme « E--1  1+ » est vérifiée sur capture (le service est le 1er octet des données).
        // Les autres positions (ex. « E--1  2+ » de GENERIQUE) restent refusées tant qu'elles ne le sont pas.
        if d != "E--1  1+" {
            return Err(inconnu());
        }
        return data
            .first()
            .copied()
            .ok_or_else(|| Refus::ServiceInconnu("données de requête vides".into()));
    }
    if reste.len() == 2 {
        return u8::from_str_radix(reste, 16).map_err(|_| inconnu());
    }
    Err(inconnu())
}

/// Autorise ou refuse une commande nommée d'un calculateur.
pub fn commande(nom: &str, desc: Option<&Cmd68k>, data: &[u8]) -> Result<(), Refus> {
    if NOMS_REFUSES.contains(&nom) {
        return Err(Refus::Nom(nom.to_string()));
    }
    let s = service(desc, data)?;
    SERVICES_AUTORISES.contains(&s).then_some(()).ok_or(Refus::Service(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(v: &[&str]) -> Cmd68k {
        Cmd68k {
            d: v.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn systeme_vci() {
        assert!(systeme(0xfe).is_ok() && systeme(0x04).is_ok());
        for c in [0x00, 0x01, 0x02, 0x03, 0x06, 0x08, 0x0a, 0x10, 0xf0, 0xff] {
            assert_eq!(systeme(c), Err(Refus::Systeme(c)));
        }
    }

    #[test]
    fn services() {
        assert!(commande("INIT", Some(&d(&["E81"])), &[]).is_ok());
        assert!(commande("LEC_TRM", Some(&d(&["E21", "E--1  1+"])), &[0xc2, 0x80, 1]).is_ok());
        assert!(commande("GENERIQUE_HN", Some(&d(&["E--1  1+"])), &[0x17, 0xff, 0]).is_ok());
        // GENERIQUE (cible puis service dans les données) : forme non vérifiée, refusée
        assert!(matches!(
            commande("GENERIQUE", Some(&d(&["E--1  2+"])), &[0x10, 0x21, 0xfe]),
            Err(Refus::ServiceInconnu(_))
        ));
        for s in [
            0x14, 0x2E, 0x3B, 0x3D, 0x30, 0x31, 0x32, 0x34, 0x36, 0x27, 0x11, 0x10, 0x12, 0x00,
        ] {
            assert_eq!(
                commande("GENERIQUE_HN", Some(&d(&["E--1  1+"])), &[s, 0]),
                Err(Refus::Service(s))
            );
            assert!(commande("GENERIQUE", Some(&d(&["E--1  2+"])), &[0x10, s]).is_err());
        }
        assert_eq!(
            commande("LECTURE_DEFAUTS", Some(&d(&["E14", "E--1  1+"])), &[0xff, 0]),
            Err(Refus::Service(0x14))
        );
    }

    #[test]
    fn refus_par_defaut() {
        assert_eq!(
            commande("EFFACE_DEFAUTS", Some(&d(&["E21"])), &[]),
            Err(Refus::Nom("EFFACE_DEFAUTS".into()))
        );
        assert!(matches!(commande("X", None, &[0x21]), Err(Refus::ServiceInconnu(_))));
        assert!(matches!(
            commande("X", Some(&d(&[])), &[0x21]),
            Err(Refus::ServiceInconnu(_))
        ));
        assert!(matches!(
            commande("X", Some(&d(&["E--1  1+"])), &[]),
            Err(Refus::ServiceInconnu(_))
        ));
        assert!(matches!(
            commande("X", Some(&d(&["Z21"])), &[]),
            Err(Refus::ServiceInconnu(_))
        ));
        assert!(matches!(
            commande("X", Some(&d(&["E--0"])), &[0x21]),
            Err(Refus::ServiceInconnu(_))
        ));
    }
}
