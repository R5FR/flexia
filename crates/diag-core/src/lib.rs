//! Session de diagnostic au-dessus d'un [`Transport`] : ouverture d'un canal vers un calculateur,
//! commandes nommées de `GPC.COMMAND`, garde lecture seule ([`garde`]), décodage des réponses.

pub mod garde;

use garde::Refus;
use psa_data::defauts::TableBits;
use psa_data::descr::{self, Cmd68k};
use psa_data::gpc::Gpc;
use psa_data::mesures::Trame;
use std::collections::BTreeMap;
use std::fmt;
use vci::{etat, Request, Response, Transport, VciError, CAL_SYSTEME};

/// Pause entre le retour au boot et le démarrage du firmware (`Sleep(100)` de GestComm).
pub const PAUSE_APRES_RESET: std::time::Duration = std::time::Duration::from_millis(100);
/// Pause après le démarrage du firmware, avant toute autre commande. Constaté le 2026-11-17 sur la C5 : sans
/// elle, la VCI mesure 0,40 V sur la prise et refuse le descripteur `C_` (état `29`) ; DiagBox laisse au moins
/// 400 ms entre `12` et le voltmètre, 1,4 s avant le premier `C_` (capture du 13/11).
pub const PAUSE_APRES_DEMARRAGE: std::time::Duration = std::time::Duration::from_millis(1000);
/// Statut de la VCI sans session calculateur ouverte ; `01 00` quand une session est ouverte.
pub const STATUT_REPOS: [u8; 2] = [0, 0];
/// Statut de la VCI pendant une session calculateur (constaté : la VCI entretient elle-même la session).
pub const STATUT_SESSION: [u8; 2] = [1, 0];

/// Commandes système de la VCI utilisées.
pub mod sys {
    /// Chargement d'un descripteur de protocole `P_*.68K`.
    pub const PROTOCOLE: u8 = 0x04;
    /// Chargement d'un descripteur de configuration `C_*.68K`.
    pub const CONFIG: u8 = 0x05;
    /// Retour au boot (réinitialisation de la VCI).
    pub const RESET: u8 = 0x07;
    /// Voltmètre.
    pub const VOLTMETRE: u8 = 0x0b;
    /// Démarrage du firmware applicatif ; répond par la chaîne de version.
    pub const DEMARRER: u8 = 0x12;
    /// Version du firmware applicatif.
    pub const VERSION: u8 = 0xfa;
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

/// Chaîne de version de la VCI : champs séparés par des zéros
/// (nom du firmware, référence, version `V…`, fabricant, version du boot).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionVci {
    pub champs: Vec<String>,
}

impl VersionVci {
    pub fn parse(data: &[u8]) -> Self {
        let champs = data
            .split(|&c| c == 0)
            .map(|c| {
                c.iter()
                    .map(|&b| {
                        if b.is_ascii_graphic() || b == b' ' {
                            b as char
                        } else {
                            '?'
                        }
                    })
                    .collect::<String>()
            })
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty())
            .collect();
        Self { champs }
    }

    /// Champ de version du firmware, ex. `V4.3.2`.
    pub fn version(&self) -> Option<&str> {
        self.champs
            .iter()
            .map(String::as_str)
            .find(|c| c.starts_with('V') && c[1..].starts_with(|d: char| d.is_ascii_digit()))
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
    /// Code KWP à 5 caractères (`P0420`), ou code méthode PSA à 3 caractères (`H7X`) pour un calculateur
    /// à champs de bits.
    pub code: String,
    /// Code KWP brut ; 0 pour un code méthode.
    pub brut: u16,
    /// Octet de statut KWP ; 0 pour un code méthode.
    pub statut: u8,
    /// Code méthode lu dans une zone en champs de bits (combiné, climatisation, afficheur…).
    pub methode: bool,
}

/// Défauts d'une réponse en champs de bits, par la table du module (voir [`psa_data::defauts`]).
pub fn decode_defauts_bits(table: &TableBits, kwp: &[u8]) -> Vec<Defaut> {
    table
        .decoder(kwp)
        .into_iter()
        .map(|code| Defaut {
            code,
            brut: 0,
            statut: 0,
            methode: true,
        })
        .collect()
}

/// Code défaut normalisé : 2 bits de lettre (P, C, B, U) puis 4 chiffres.
pub fn code_defaut(brut: u16) -> String {
    let lettre = ['P', 'C', 'B', 'U'][(brut >> 14) as usize];
    format!("{lettre}{:04X}", brut & 0x3FFF)
}

/// Champs de l'identification d'un calculateur (réponse `61 80 …`), tels que Lexia les affiche.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identification {
    /// « Référence PSA » : 10 chiffres.
    pub reference_psa: String,
    /// Code du fournisseur (ex. 5 = SAGEM/JCAE dans la base).
    pub fournisseur: u8,
    /// « Référence complémentaire » : 10 chiffres ; absente si ces octets ne sont pas des chiffres (ABS).
    pub reference_complementaire: Option<String>,
    /// « Version diagnostic » : 2 chiffres hexadécimaux.
    pub version_diagnostic: String,
}

/// Décode l'identification `61 80 | réf. PSA (5 o) | 00 | fournisseur | réf. complémentaire (5 o) | 4 o | version diag`.
/// Constaté sur l'injection S2000PM1 contre l'écran de Lexia du 2026-11-13 ; supposé identique pour les
/// autres calculateurs qui répondent `61 80` (ABS : même début, référence complémentaire non numérique).
/// Rend `None` si la référence PSA n'est pas une suite de chiffres : la réponse reste alors à lire en brut.
pub fn decode_identification(kwp: &[u8]) -> Option<Identification> {
    if kwp.len() < 19 || kwp[..2] != [0x61, 0x80] {
        return None;
    }
    let chiffres = |octets: &[u8]| {
        let s: String = octets.iter().map(|b| format!("{b:02X}")).collect();
        s.bytes().all(|c| c.is_ascii_digit()).then_some(s)
    };
    Some(Identification {
        reference_psa: chiffres(&kwp[2..7])?,
        fournisseur: kwp[8],
        reference_complementaire: chiffres(&kwp[9..14]),
        version_diagnostic: format!("{:02X}", kwp[18]),
    })
}

/// Deux chiffres pour un octet BCD, comme Lexia : dizaines × 10 + unités, tronqué à deux caractères.
/// Un octet non BCD déborde (`ff` → « 16 », `e6` → « 14 ») : c'est ce que montre l'écran de Lexia
/// pour une zone que le calculateur n'a pas renseignée.
fn bcd2(octet: u8) -> String {
    let v = u32::from(octet >> 4) * 10 + u32::from(octet & 0x0F);
    format!("{v:02}").chars().take(2).collect()
}

/// Date `jj mm aa` en octets décimaux.
fn date(octets: &[u8]) -> String {
    format!("{:02}/{:02}/20{:02}", octets[0], octets[1], octets[2])
}

/// « Date de dernière intervention » : octets 6 à 8 de la zone `61 cf` de l'injection S2000 (constaté).
pub fn decode_date_intervention(kwp: &[u8]) -> Option<String> {
    (kwp.len() >= 9 && kwp[..2] == [0x61, 0xcf]).then(|| date(&kwp[6..9]))
}

/// Zone `61 fe` de l'injection S2000 (table `TRAMEFE` du module) : (indice d'évolution, référence logiciel).
/// Positions constatées : indice aux octets 13 et 14, référence aux octets 23 à 25, en BCD.
pub fn decode_zone_fe(kwp: &[u8]) -> Option<(String, String)> {
    if kwp.len() < 26 || kwp[..2] != [0x61, 0xfe] {
        return None;
    }
    let indice = format!("{}{}", bcd2(kwp[13]), bcd2(kwp[14]));
    let reference = format!("96{}{}{}80", bcd2(kwp[23]), bcd2(kwp[24]), bcd2(kwp[25]));
    Some((indice, reference))
}

/// Identification complète : la réponse brute et les champs décodés, nommés comme dans la base
/// (`FOURNISSEUR`, `REF_PSA`, `REFERENCE_SOFT`, `REF_LOG`, `EDITIONSOFT`, `DATEDERINTERV`, `VERSION_DIAG`),
/// dans l'ordre de l'écran de Lexia.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentificationComplete {
    pub reponse: ReponseEcu,
    pub champs: Vec<(&'static str, String)>,
}

/// Libellés d'un écran d'identification : chaque champ avec son libellé de la base (sinon son nom), et le
/// fournisseur avec son nom plutôt que son code quand la base le connaît.
pub fn habiller_identification(
    champs: &[(&'static str, String)],
    habillages: &std::collections::HashMap<String, psa_data::libelles::Habillage>,
) -> Vec<(String, String)> {
    champs
        .iter()
        .map(|(nom, valeur)| {
            let h = habillages.get(*nom);
            let libelle = h
                .map(|h| h.libelle.clone())
                .filter(|l| !l.is_empty())
                .unwrap_or_else(|| nom.to_string());
            let etat = h.and_then(|h| valeur.parse::<f64>().ok().and_then(|v| h.etat(v)));
            (libelle, etat.map_or_else(|| valeur.clone(), str::to_string))
        })
        .collect()
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
                methode: false,
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

    /// Initialise la VCI comme GestComm (`CUSBCommDriver`) : retour au boot, pause de 100 ms, puis démarrage
    /// du firmware applicatif. Comme lui, l'état rendu par le retour au boot n'est pas examiné (`0c` ou `03`
    /// constatés). Le retour au boot met aussi fin à une session calculateur restée ouverte dans la VCI.
    pub fn connecter(&mut self) -> Result<VersionVci> {
        self.systeme(sys::RESET, &[])?;
        std::thread::sleep(PAUSE_APRES_RESET);
        let rep = self.systeme(sys::DEMARRER, &[])?;
        if !matches!(rep.etat, etat::ACCEPTE | etat::REPONSE) {
            return Err(Error::Etat {
                etat: rep.etat,
                contexte: "démarrage du firmware de la VCI".into(),
            });
        }
        std::thread::sleep(PAUSE_APRES_DEMARRAGE);
        Ok(VersionVci::parse(&rep.data))
    }

    /// Version du firmware applicatif de la VCI.
    pub fn version(&mut self) -> Result<VersionVci> {
        let rep = self.systeme(sys::VERSION, &[])?;
        if rep.etat != etat::REPONSE {
            return Err(Error::Etat {
                etat: rep.etat,
                contexte: "version de la VCI".into(),
            });
        }
        Ok(VersionVci::parse(&rep.data))
    }

    /// Voltmètre de la VCI (type de tension `04`, comme Lexia), en volts. La VCI rend la partie entière puis
    /// les centièmes : tension = entier + centièmes × 0,01 (`GestComm!_MesureTensionMVCI`).
    pub fn voltmetre(&mut self) -> Result<f64> {
        let rep = self.systeme(sys::VOLTMETRE, &[0x04])?;
        match (rep.etat, rep.data.as_slice()) {
            (etat::REPONSE, &[entier, centiemes]) => Ok(f64::from(entier) + f64::from(centiemes) * 0.01),
            (etat::REPONSE, d) => Err(Error::Reponse(format!("voltmètre : {} octets", d.len()))),
            (e, _) => Err(Error::Etat {
                etat: e,
                contexte: "voltmètre".into(),
            }),
        }
    }

    /// Ouvre le canal d'un type de calculateur : statut, chargement des descripteurs `C_` puis `P_*.68K`, `INIT`.
    pub fn ouvrir(&mut self, ecu_type_id: i64) -> Result<Canal> {
        let (canal, config, protocole) = self.descripteurs(ecu_type_id)?;
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
        self.commande(&canal, "INIT", &[])?;
        Ok(canal)
    }

    /// Calculateur joint à travers la passerelle (la BSI) dont la session est ouverte, comme le test global de
    /// Lexia (constaté le 2026-11-13 sur la C5 : combiné, climatisation, afficheur, sac gonflable) : statut,
    /// puis chargement du seul descripteur `P_*.68K` du calculateur, sans nouvel `INIT`. La VCI répond par le
    /// nombre d'enregistrements de 45 octets chargés (`9a` = 154 pour les 6 930 octets de `p_comb7a`). Les
    /// commandes suivantes vont au calculateur ; la session de la passerelle reste ouverte.
    pub fn joindre(&mut self, passerelle: &Canal, ecu_type_id: i64) -> Result<Canal> {
        let (canal, _, protocole) = self.descripteurs(ecu_type_id)?;
        if canal.code_cal != passerelle.code_cal {
            return Err(Error::Donnee(format!(
                "le type {ecu_type_id} est sur le canal {:02x}, la passerelle sur {:02x}",
                canal.code_cal, passerelle.code_cal
            )));
        }
        self.statut()?;
        let rep = self.systeme(sys::PROTOCOLE, &protocole)?;
        let attendu = (protocole.len() / 45) as u8;
        if rep.etat != etat::REPONSE || rep.data.first() != Some(&attendu) {
            return Err(Error::Etat {
                etat: rep.etat,
                contexte: format!(
                    "chargement du descripteur P_ du type {ecu_type_id} (réponse {:02x?})",
                    rep.data
                ),
            });
        }
        Ok(canal)
    }

    /// Fin du dialogue avec un calculateur joint : sa commande `FIN_DIAG` (réponse `c2` du calculateur). La
    /// session de la passerelle reste ouverte (statut `01 00`) ; elle se ferme ensuite par [`Session::fermer`].
    /// Pas de relecture du statut ici : dans la capture, Lexia la fait ou non selon le calculateur suivant.
    pub fn quitter(&mut self, canal: Canal) -> Result<()> {
        self.commande(&canal, "FIN_DIAG", &[])?;
        Ok(())
    }

    /// Défauts d'un calculateur à champs de bits : la commande de sa table (ex. `LECTURE_DEFAUTS`, `c3` venant
    /// de la base ; `LEC_TRM` + `83` pour l'afficheur), puis le décodage par la table.
    pub fn lire_defauts_bits(&mut self, canal: &Canal, table: &TableBits) -> Result<(ReponseEcu, Vec<Defaut>)> {
        let rep = self.commande(canal, &table.commande, &table.donnees)?;
        let defauts = decode_defauts_bits(table, rep.kwp());
        Ok((rep, defauts))
    }

    /// Canal (sans échange) et descripteurs `C_` et `P_` d'un type de calculateur ; règle aussi les délais de
    /// la VCI pour son canal (table `.PC` du protocole).
    fn descripteurs(&mut self, ecu_type_id: i64) -> Result<(Canal, Vec<u8>, Vec<u8>)> {
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
        // temporisations des commandes : table `.PC` du protocole (même format que SUPER.PC), lignes du canal
        let tempos: Vec<(u8, u32)> = prot
            .files
            .iter()
            .filter(|(n, _)| n.to_ascii_uppercase().ends_with(".PC"))
            .flat_map(|(_, b)| descr::parse_pc(b))
            .filter(|r| r.classe == code_cal)
            .map(|r| (r.code, r.delai))
            .collect();
        self.vci.definir_tempos(code_cal, &tempos);
        let canal = Canal {
            ecu_type_id,
            code_cal,
            desc,
        };
        Ok((canal, config, protocole))
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

    /// Fin de communication avec le calculateur, puis lecture du statut de la VCI.
    /// Renvoie `true` si la VCI n'a plus de session ouverte (statut `00 00`). Tant qu'une session est
    /// ouverte, c'est la VCI qui l'entretient auprès du calculateur : elle ne s'éteint pas d'elle-même.
    pub fn fermer(&mut self, canal: Canal) -> Result<bool> {
        self.commande(&canal, "FIN_DIAG", &[])?;
        Ok(self.statut()? == STATUT_REPOS)
    }

    pub fn identification(&mut self, canal: &Canal) -> Result<ReponseEcu> {
        self.commande(canal, "IDENTIFICATION", &[])
    }

    /// Identification telle que Lexia l'affiche. Pour les injections S2000PM1, lit aussi comme lui les
    /// zones `cf 00` (date de dernière intervention) et `fe` (référence logiciel, indice d'évolution) ;
    /// une zone qui ne répond pas est simplement absente du résultat.
    pub fn identification_complete(&mut self, canal: &Canal) -> Result<IdentificationComplete> {
        let reponse = self.identification(canal)?;
        let mut champs = Vec::new();
        if let Some(id) = decode_identification(reponse.kwp()) {
            let s2000 = self
                .gpc
                .ecu_type(canal.ecu_type_id)
                .is_some_and(|t| t.name == "S2000PM1");
            let zone = |s: &mut Self, lid: &[u8]| s2000.then(|| s.commande(canal, "LEC_TRM", lid).ok()).flatten();
            let date = zone(self, &[0xcf, 0x00]).and_then(|r| decode_date_intervention(r.kwp()));
            let fe = zone(self, &[0xfe]).and_then(|r| decode_zone_fe(r.kwp()));
            champs.push(("FOURNISSEUR", id.fournisseur.to_string()));
            champs.push(("REF_PSA", id.reference_psa));
            if let Some(r) = id.reference_complementaire {
                champs.push(("REFERENCE_SOFT", r));
            }
            if let Some((indice, reference)) = fe {
                champs.push(("REF_LOG", reference));
                champs.push(("EDITIONSOFT", indice));
            }
            if let Some(d) = date {
                champs.push(("DATEDERINTERV", d));
            }
            champs.push(("VERSION_DIAG", id.version_diagnostic));
        }
        Ok(IdentificationComplete { reponse, champs })
    }

    /// Défauts mémorisés (service `17`, réponse `57`). Lexia passe par la commande générique avec `17 FF 00`
    /// sur l'injection S2000PM1 et le sac gonflable, et par `LECTURE_DEFAUTS` (données `FF 00` de la base)
    /// sur l'ABS : les deux sont constatés sur la capture. Pour un autre calculateur, `LECTURE_DEFAUTS` est
    /// préférée si son descripteur envoie bien le service `17`, sinon la commande générique.
    pub fn lire_defauts(&mut self, canal: &Canal) -> Result<Vec<Defaut>> {
        let nom = self.gpc.ecu_type(canal.ecu_type_id).map_or("", |t| t.name.as_str());
        let par_generique = matches!(nom, "S2000PM1" | "SAC_TEMIC");
        let dediee = self.gpc.command(canal.ecu_type_id, "LECTURE_DEFAUTS").is_some_and(|c| {
            let d = canal.desc.get(&format!("{:02X}", c.code_com)).and_then(|d| d.d.first());
            d.is_some_and(|d| d == "E17")
        });
        let rep = if dediee && !par_generique {
            self.commande(canal, "LECTURE_DEFAUTS", &[])?
        } else {
            self.commande(canal, "GENERIQUE_HN", &[0x17, 0xFF, 0x00])?
        };
        decode_defauts(rep.kwp())
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
    fn version_vci() {
        let v = VersionVci::parse(b"APPLI_X\0REF1 \0V1.2.3\0@FAB\x0001.02\0");
        assert_eq!(v.champs, ["APPLI_X", "REF1", "V1.2.3", "@FAB", "01.02"]);
        assert_eq!(v.version(), Some("V1.2.3"));
        assert_eq!(VersionVci::parse(b"").version(), None);
    }

    #[test]
    fn identification() {
        let mut kwp = vec![
            0x61, 0x80, 0x96, 0x12, 0x34, 0x56, 0x80, 0x00, 0x05, 0x96, 0x65, 0x43, 0x21, 0x80,
        ];
        kwp.extend_from_slice(&[0xff, 0xff, 0xff, 0xff, 0x0d, 0x01, 0xff]);
        let id = decode_identification(&kwp).unwrap();
        assert_eq!(id.reference_psa, "9612345680");
        assert_eq!(id.fournisseur, 5);
        assert_eq!(id.reference_complementaire.as_deref(), Some("9665432180"));
        assert_eq!(id.version_diagnostic, "0D");
        assert!(decode_identification(&kwp[..18]).is_none());
        kwp[10] = 0xAB; // référence complémentaire non numérique : les autres champs restent
        let id = decode_identification(&kwp).unwrap();
        assert_eq!(
            (id.reference_psa.as_str(), id.reference_complementaire),
            ("9612345680", None)
        );
        kwp[3] = 0xAB; // référence PSA non numérique : pas une identification de cette forme
        assert!(decode_identification(&kwp).is_none());
        kwp[1] = 0x81;
        assert!(decode_identification(&kwp).is_none());
    }

    #[test]
    fn zones_s2000() {
        assert_eq!(
            (bcd2(0x16), bcd2(0x00), bcd2(0xff), bcd2(0xe6)),
            ("16".into(), "00".into(), "16".into(), "14".into())
        );
        let mut fe = vec![0x61, 0xfe];
        fe.extend_from_slice(&[0; 24]);
        fe[13] = 0x12;
        fe[14] = 0x34;
        fe[23..26].copy_from_slice(&[0x11, 0x22, 0x33]);
        assert_eq!(
            decode_zone_fe(&fe),
            Some(("1234".to_string(), "9611223380".to_string()))
        );
        assert!(decode_zone_fe(&fe[..25]).is_none());
        let cf = [0x61, 0xcf, 0, 0, 0, 0, 0x14, 0x09, 0x1a];
        assert_eq!(decode_date_intervention(&cf).as_deref(), Some("20/09/2026"));
        assert!(decode_date_intervention(&cf[..8]).is_none());
    }

    #[test]
    fn defauts() {
        let d = decode_defauts(&[0x57, 0x01, 0x04, 0x20, 0x28]).unwrap();
        assert_eq!(
            d,
            [Defaut {
                code: "P0420".into(),
                brut: 0x0420,
                statut: 0x28,
                methode: false,
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
