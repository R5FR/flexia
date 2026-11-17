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
pub const NOMS_REFUSES: [&str; 13] = [
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
];

/// `MAPPING` de la BSI : seule commande autorisée hors [`SERVICES_AUTORISES`], et seulement sous cette forme.
/// Service `35` (RequestUpload : téléchargement *depuis* le calculateur, une lecture) ; Lexia l'envoie à chaque
/// ouverture de la BSI pour connaître la composition de ses zones (capture du 2026-11-13, 31 échanges par
/// session). Enregistrements `D` du descripteur `P_BSIGEN` (constatés) : `35`, les quatre octets des données,
/// puis `11`.
pub const MAPPING_D: [&str; 6] = ["E35", "E--1  1 ", "E--1  2 ", "E--1  3 ", "E--1  4 ", "E11"];
/// Familles de zones que `MAPPING` peut demander (`LISTEINI.INI`, `[BSI] FamillesLID`).
pub const FAMILLES_MAPPING: [u8; 5] = [0xC0, 0xB0, 0xD0, 0xA0, 0x83];

/// Autorise `MAPPING` si son descripteur et ses données ont exactement la forme vue sur la capture :
/// `famille 00 00 bloc`, bloc de 1 à 255.
fn mapping(desc: Option<&Cmd68k>, data: &[u8]) -> Result<(), Refus> {
    let forme = desc.is_some_and(|c| c.d.iter().map(String::as_str).eq(MAPPING_D));
    if !forme {
        return Err(Refus::ServiceInconnu("MAPPING : descripteur différent de celui de la BSI".into()));
    }
    match data {
        [f, 0, 0, b] if FAMILLES_MAPPING.contains(f) && *b >= 1 => Ok(()),
        _ => Err(Refus::ServiceInconnu(format!("MAPPING : données {data:02x?} hors de la forme famille 00 00 bloc"))),
    }
}

/// Débuts de nom refusés : écritures, effacements, téléchargements, pilotages (ex. `TELECODAGE1`,
/// `ECR_CONFIG`, `ECRITURE_ZONE_HIST`, `WRITE_MODE_FONC`, `EFFACE_FLASH_STATUS`).
pub const PREFIXES_REFUSES: [&str; 12] = [
    "TELECODAGE",
    "ECR",
    "WRITE",
    "EFFACE",
    "RAZ",
    "RESET",
    "TELECHARG",
    "PILOTAGE",
    "ACTIVATION",
    "SECURITY_ACCESS",
    "DEMARRE_ROUTINE",
    "ARRET_ROUTINE",
];

/// Vrai si le nom de la commande suffit à la refuser, sans regarder son service.
pub fn nom_refuse(nom: &str) -> bool {
    NOMS_REFUSES.contains(&nom) || PREFIXES_REFUSES.iter().any(|p| nom.starts_with(p))
}

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

/// Service d'une trame KWP2000 complète à adressage physique : `8L cible source SID … somme`, avec
/// L = nombre d'octets de données et somme = somme des octets précédents modulo 256. `None` sinon.
pub fn service_trame_complete(data: &[u8]) -> Option<u8> {
    let (&somme, corps) = data.split_last()?;
    let longueur = usize::from(*corps.first()? & 0x3F);
    let coherent = corps[0] & 0xC0 == 0x80
        && longueur >= 1
        && corps.len() == 3 + longueur
        && corps.iter().fold(0u8, |a, b| a.wrapping_add(*b)) == somme;
    coherent.then(|| corps[3])
}

/// Autorise ou refuse une commande nommée d'un calculateur.
pub fn commande(nom: &str, desc: Option<&Cmd68k>, data: &[u8]) -> Result<(), Refus> {
    if nom_refuse(nom) {
        return Err(Refus::Nom(nom.to_string()));
    }
    if nom == "MAPPING" {
        return mapping(desc, data);
    }
    // Certains protocoles (BSI) n'ont pas de descripteur pour INIT : la base fournit la trame complète
    // (`81 d0 f1 81 c3`, constaté sur capture). Seul INIT a droit à cette forme, et elle est vérifiée.
    let s = match (desc, nom) {
        (None, "INIT") => service_trame_complete(data)
            .ok_or_else(|| Refus::ServiceInconnu("INIT sans descripteur : trame complète attendue".into()))?,
        _ => service(desc, data)?,
    };
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
    fn init_par_trame_complete() {
        let init = [0x81, 0xd0, 0xf1, 0x81, 0xc3];
        assert_eq!(service_trame_complete(&init), Some(0x81));
        assert!(commande("INIT", None, &init).is_ok());
        // même trame sous un autre nom : refusée
        assert!(matches!(
            commande("LEC_TRM", None, &init),
            Err(Refus::ServiceInconnu(_))
        ));
        // somme fausse, longueur fausse, adressage fonctionnel, trame vide
        assert_eq!(service_trame_complete(&[0x81, 0xd0, 0xf1, 0x81, 0xc4]), None);
        assert_eq!(service_trame_complete(&[0x82, 0xd0, 0xf1, 0x81, 0xc4]), None);
        assert_eq!(service_trame_complete(&[0xc1, 0xd0, 0xf1, 0x81, 0x03]), None);
        assert_eq!(service_trame_complete(&[]), None);
        assert!(matches!(commande("INIT", None, &[]), Err(Refus::ServiceInconnu(_))));
        // trame complète bien formée mais service d'écriture (14 = effacement) : refusée
        let efface = [0x83, 0xd0, 0xf1, 0x14, 0xff, 0x00, 0x57];
        assert_eq!(service_trame_complete(&efface), Some(0x14));
        assert_eq!(commande("INIT", None, &efface), Err(Refus::Service(0x14)));
    }

    #[test]
    fn noms_refuses() {
        for nom in [
            "TELECODAGE",
            "TELECODAGE1",
            "ECR_CONFIG",
            "ECRITURE_ZONE_HIST",
            "WRITE_MODE_FONC",
            "RESET_ECU",
        ] {
            assert!(nom_refuse(nom), "{nom}");
            assert_eq!(commande(nom, Some(&d(&["E21"])), &[]), Err(Refus::Nom(nom.into())));
        }
        for nom in [
            "INIT",
            "FIN_DIAG",
            "TP",
            "IDENTIFICATION",
            "IDENTIFICATION_1",
            "LEC_TRM",
            "LECTURE_DEFAUTS",
        ] {
            assert!(!nom_refuse(nom), "{nom}");
        }
        // un nom de lecture ne protège pas : le service est toujours contrôlé
        assert_eq!(commande("LEC_TRM", Some(&d(&["E3B"])), &[]), Err(Refus::Service(0x3B)));
    }

    #[test]
    fn mapping_de_la_bsi() {
        let bsi = d(&MAPPING_D);
        assert!(commande("MAPPING", Some(&bsi), &[0xc0, 0, 0, 1]).is_ok());
        assert!(commande("MAPPING", Some(&bsi), &[0x83, 0, 0, 0x0d]).is_ok());
        // famille inconnue, bloc 0, bloc au-delà de 255, longueur différente
        for data in [&[0x84, 0, 0, 1][..], &[0xc0, 0, 0, 0], &[0xc0, 0, 1, 0], &[0xc0, 0, 0], &[0xc0, 0, 0, 1, 0x11]] {
            assert!(matches!(commande("MAPPING", Some(&bsi), data), Err(Refus::ServiceInconnu(_))), "{data:02x?}");
        }
        // descripteur d'une autre forme (même service 35, ou téléchargement vers le calculateur) : refusé
        for autre in [d(&["E35", "E--1  1+"]), d(&["E34", "E--1  1 ", "E--1  2 ", "E--1  3 ", "E--1  4 ", "E11"])] {
            assert!(commande("MAPPING", Some(&autre), &[0xc0, 0, 0, 1]).is_err());
        }
        assert!(commande("MAPPING", None, &[0xc0, 0, 0, 1]).is_err());
        // le service 35 reste refusé sous tout autre nom
        assert_eq!(commande("LEC_TRM", Some(&bsi), &[0xc0, 0, 0, 1]), Err(Refus::Service(0x35)));
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
