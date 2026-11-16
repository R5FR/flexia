//! Session de diagnostic au-dessus d'un [`Transport`] : ouverture d'un canal vers un calculateur,
//! commandes nommées de `GPC.COMMAND`, garde lecture seule ([`garde`]), décodage des réponses.

pub mod garde;

use garde::Refus;
use psa_data::descr::{self, Cmd68k};
use psa_data::gpc::Gpc;
use psa_data::mesures::Trame;
use std::collections::BTreeMap;
use std::fmt;
use vci::{etat, Request, Response, Transport, VciError, CAL_SYSTEME};

/// Commandes système de la VCI utilisées.
pub mod sys {
    /// Chargement d'un descripteur de protocole `P_*.68K`.
    pub const PROTOCOLE: u8 = 0x04;
    /// Chargement d'un descripteur de configuration `C_*.68K`.
    pub const CONFIG: u8 = 0x05;
    /// Statut de communication.
    pub const STATUT: u8 = 0xfe;
}

#[derive(Debug)]
pub enum Error {
    Vci(VciError),
    /// Refusé par la garde lecture seule ; rien n'a été envoyé.
    Refus(Refus),
    /// Donnée absente de la base (type de calculateur, commande, protocole).
    Donnee(String),
    /// La VCI a répondu par un état d'échec.
    Etat {
        etat: u8,
        contexte: String,
    },
    /// Réponse négative du calculateur (`7F service code`).
    Negative {
        service: u8,
        code: u8,
    },
    /// Réponse de forme inattendue.
    Reponse(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Vci(e) => write!(f, "{e}"),
            Error::Refus(r) => write!(f, "refus : {r}"),
            Error::Donnee(m) => write!(f, "donnée absente : {m}"),
            Error::Etat { etat, contexte } => write!(f, "état VCI {etat:02x} ({contexte})"),
            Error::Negative { service, code } => {
                write!(
                    f,
                    "réponse négative du calculateur au service {service:02x} : code {code:02x}"
                )
            }
            Error::Reponse(m) => write!(f, "réponse inattendue : {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<VciError> for Error {
    fn from(e: VciError) -> Self {
        Error::Vci(e)
    }
}

impl From<Refus> for Error {
    fn from(r: Refus) -> Self {
        Error::Refus(r)
    }
}

/// Canal ouvert vers un calculateur.
#[derive(Debug, Clone)]
pub struct Canal {
    pub ecu_type_id: i64,
    /// Code cal du canal sur la VCI.
    pub code_cal: u8,
    /// Enregistrements du `P_*.68K` par Code Com (hexadécimal majuscule).
    desc: BTreeMap<String, Cmd68k>,
}

/// Réponse d'un calculateur, préfixe de la VCI compris.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReponseEcu {
    /// `01 | adresse | longueur u16 | 05 | réponse KWP`.
    pub brut: Vec<u8>,
}

impl ReponseEcu {
    /// Adresse du calculateur qui a répondu.
    pub fn adresse(&self) -> u8 {
        self.brut[1]
    }
    /// Réponse KWP2000, sans en-tête ni somme de contrôle.
    pub fn kwp(&self) -> &[u8] {
        &self.brut[5..]
    }
}

/// Code défaut lu dans le calculateur.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Defaut {
    /// Code à 5 caractères, ex. `P0420`.
    pub code: String,
    pub brut: u16,
    pub statut: u8,
}

/// Code défaut normalisé : 2 bits de lettre (P, C, B, U) puis 4 chiffres.
pub fn code_defaut(brut: u16) -> String {
    let lettre = ['P', 'C', 'B', 'U'][(brut >> 14) as usize];
    format!("{lettre}{:04X}", brut & 0x3FFF)
}

/// Décode une réponse `57 n (code u16, statut)×n` (ReadStatusOfDTC).
pub fn decode_defauts(kwp: &[u8]) -> Result<Vec<Defaut>> {
    let (&[0x57, n], reste) = kwp
        .split_at_checked(2)
        .ok_or_else(|| Error::Reponse("défauts : trop court".into()))?
    else {
        return Err(Error::Reponse(format!(
            "défauts : service {:02x} au lieu de 57",
            kwp[0]
        )));
    };
    if reste.len() != 3 * n as usize {
        return Err(Error::Reponse(format!(
            "défauts : {n} annoncés, {} octets",
            reste.len()
        )));
    }
    Ok(reste
        .chunks(3)
        .map(|c| {
            let brut = u16::from_be_bytes([c[0], c[1]]);
            Defaut {
                code: code_defaut(brut),
                brut,
                statut: c[2],
            }
        })
        .collect())
}

pub struct Session<'a, T: Transport> {
    vci: T,
    gpc: &'a Gpc,
}

impl<'a, T: Transport> Session<'a, T> {
    pub fn new(vci: T, gpc: &'a Gpc) -> Self {
        Self { vci, gpc }
    }

    pub fn transport(&self) -> &T {
        &self.vci
    }

    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.vci
    }

    /// Commande système de la VCI (Code cal 00), si la garde l'autorise.
    pub fn systeme(&mut self, code_com: u8, data: &[u8]) -> Result<Response> {
        garde::systeme(code_com)?;
        let mut rep = None;
        for req in Request::blocs(CAL_SYSTEME, code_com, data) {
            rep = Some(self.vci.exchange(&req)?);
        }
        Ok(rep.expect("au moins un bloc"))
    }

    /// Statut de communication (2 octets).
    pub fn statut(&mut self) -> Result<Vec<u8>> {
        Ok(self.systeme(sys::STATUT, &[])?.data)
    }

    /// Ouvre le canal d'un type de calculateur : statut, chargement des descripteurs `C_` puis `P_*.68K`, `INIT`.
    pub fn ouvrir(&mut self, ecu_type_id: i64) -> Result<Canal> {
        let manque = |quoi: &str| Error::Donnee(format!("{quoi} du type de calculateur {ecu_type_id}"));
        let prot = self.gpc.protocol_of(ecu_type_id).ok_or_else(|| manque("protocole"))?;
        let code_cal = self
            .gpc
            .commands_of(ecu_type_id)
            .next()
            .ok_or_else(|| manque("commandes"))?
            .code_cal;
        let fichier = |prefixe: &str| {
            prot.files
                .iter()
                .find(|(n, _)| {
                    let n = n.to_ascii_uppercase();
                    n.starts_with(prefixe) && n.ends_with(".68K")
                })
                .map(|(_, b)| {
                    b.iter()
                        .copied()
                        .filter(|&c| c != b'\r' && c != b'\n')
                        .collect::<Vec<u8>>()
                })
        };
        let config = fichier("C_").ok_or_else(|| manque("descripteur C_*.68K"))?;
        let protocole = fichier("P_").ok_or_else(|| manque("descripteur P_*.68K"))?;
        let (desc, _) = descr::commandes_68k(&descr::parse_68k(&protocole));

        self.statut()?;
        for (com, data, nom) in [(sys::CONFIG, &config, "C_"), (sys::PROTOCOLE, &protocole, "P_")] {
            let rep = self.systeme(com, data)?;
            if !matches!(rep.etat, etat::REPONSE | etat::ACCEPTE) {
                return Err(Error::Etat {
                    etat: rep.etat,
                    contexte: format!("chargement du descripteur {nom}"),
                });
            }
        }
        let canal = Canal {
            ecu_type_id,
            code_cal,
            desc,
        };
        self.commande(&canal, "INIT", &[])?;
        Ok(canal)
    }

    /// Envoie une commande nommée ; `extra` s'ajoute aux données fixes de la base.
    /// Refusée avant tout envoi si la garde lecture seule ne l'autorise pas.
    pub fn commande(&mut self, canal: &Canal, nom: &str, extra: &[u8]) -> Result<ReponseEcu> {
        let cmd = self
            .gpc
            .command(canal.ecu_type_id, nom)
            .ok_or_else(|| Error::Donnee(format!("commande {nom} du type de calculateur {}", canal.ecu_type_id)))?;
        let mut data = cmd.data.clone();
        data.extend_from_slice(extra);
        garde::commande(nom, canal.desc.get(&format!("{:02X}", cmd.code_com)), &data)?;

        let rep = self.vci.exchange(&Request::new(cmd.code_cal, cmd.code_com, data))?;
        if rep.etat != etat::REPONSE {
            return Err(Error::Etat {
                etat: rep.etat,
                contexte: nom.to_string(),
            });
        }
        let b = &rep.data;
        if b.len() < 6 || b[0] != 0x01 || b[4] != 0x05 || usize::from(u16::from_le_bytes([b[2], b[3]])) != b.len() - 5 {
            return Err(Error::Reponse(format!("{nom} : préfixe de réponse invalide")));
        }
        if let [0x7F, service, code, ..] = b[5..] {
            return Err(Error::Negative { service, code });
        }
        Ok(ReponseEcu { brut: rep.data })
    }

    /// Fin de communication avec le calculateur.
    pub fn fermer(&mut self, canal: Canal) -> Result<()> {
        self.commande(&canal, "FIN_DIAG", &[]).map(|_| ())
    }

    pub fn identification(&mut self, canal: &Canal) -> Result<ReponseEcu> {
        self.commande(canal, "IDENTIFICATION", &[])
    }

    /// Défauts mémorisés : `17 FF 00` par la commande générique, comme Lexia sur l'injection S2000PM1.
    pub fn lire_defauts(&mut self, canal: &Canal) -> Result<Vec<Defaut>> {
        decode_defauts(self.commande(canal, "GENERIQUE_HN", &[0x17, 0xFF, 0x00])?.kwp())
    }

    /// Lit une trame de mesures (`LEC_TRM` + requête de la trame) et décode ses fiches.
    pub fn lire_mesures<'t>(&mut self, canal: &Canal, trame: &'t Trame) -> Result<Vec<(&'t str, Option<f64>)>> {
        if trame.requete.is_empty() {
            return Err(Error::Donnee(format!("requête de la trame {}", trame.fonction)));
        }
        let rep = self.commande(canal, "LEC_TRM", &trame.requete)?;
        if rep.kwp().first() != Some(&0x61) {
            return Err(Error::Reponse(format!(
                "mesures : service {:02x?} au lieu de 61",
                rep.kwp().first()
            )));
        }
        Ok(trame.decode(&rep.brut))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defauts() {
        let d = decode_defauts(&[0x57, 0x01, 0x04, 0x20, 0x28]).unwrap();
        assert_eq!(
            d,
            [Defaut {
                code: "P0420".into(),
                brut: 0x0420,
                statut: 0x28
            }]
        );
        assert!(decode_defauts(&[0x57, 0x00]).unwrap().is_empty());
        assert!(decode_defauts(&[0x57, 0x02, 0x04, 0x20, 0x28]).is_err());
        assert!(decode_defauts(&[0x58, 0x00]).is_err());
        assert!(decode_defauts(&[0x57]).is_err());
        assert_eq!(
            (code_defaut(0x4123).as_str(), code_defaut(0xC100).as_str()),
            ("C0123", "U0100")
        );
    }
}
