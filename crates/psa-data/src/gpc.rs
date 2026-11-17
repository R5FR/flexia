//! Base véhicule `GPC.FDB` : véhicule → famille → calculateur → type de calculateur,
//! commandes nommées (`COMMAND`) et fichiers de protocole (`PROTOCOL`).
//!
//! `COMMAND.COMECU` = Code cal (canal de la VCI), `COMMAND.COMCODE` = Code Com.
//! Un même Code Com désigne des commandes différentes selon le type de calculateur.

use crate::ods10::{col, Database, Row, Value};
use crate::{Error, Result};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

/// Type de calculateur (`ECUTYPE`) ; `id` est le `n` de `CAL<n>.DLL`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EcuType {
    pub id: i64,
    pub name: String,
    pub marque: String,
    pub comment: String,
    pub prot_id: Option<i64>,
    /// Fichier `.EQU` associé (table de mesures générique).
    pub equ_fname: String,
}

/// Commande nommée d'un type de calculateur.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub id: i64,
    pub ecu_type_id: i64,
    pub name: String,
    /// Canal de la VCI (1er octet de l'en-tête).
    pub code_cal: u8,
    pub code_com: u8,
    /// Données fixes de la requête (peut être vide).
    pub data: Vec<u8>,
    /// `D` données fixes, `+` données fournies par l'appelant, `P`, `F`.
    pub data_type: String,
}

/// Jeu de fichiers d'un protocole (`P_*.68K`, `C_*.68K`, `.pro`, `.PC`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Protocol {
    pub id: i64,
    pub type_id: Option<i64>,
    /// (nom, contenu) des fichiers présents, dans l'ordre 1 à 5.
    pub files: Vec<(String, Vec<u8>)>,
}

/// Calculateur d'une famille (`ECU`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ecu {
    pub id: i64,
    pub ecu_type_id: i64,
    pub fam_id: i64,
    pub in_global_test: bool,
}

/// Famille de calculateurs d'un véhicule (`FAMILY` + nom de `FAMTYPE`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Family {
    pub id: i64,
    pub veh_id: i64,
    /// Nom du type de famille, ex. `INJ`, `BSI`.
    pub name: String,
    /// Adresse cible sur la ligne de diagnostic (hexadécimal), ex. `10`.
    pub target: String,
    pub mux: bool,
    /// Famille passerelle (`FAMTYPE.FAMGATEWAY = 1`) : la BSI (`BMF`), par laquelle passent les calculateurs
    /// d'habitacle.
    pub passerelle: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vehicule {
    pub id: i64,
    /// Code de la plate-forme, ex. `X7`.
    pub com_type: String,
    pub mux: bool,
}

/// Contenu utile de `GPC.FDB`, chargé en mémoire.
pub struct Gpc {
    pub vehicules: Vec<Vehicule>,
    pub families: Vec<Family>,
    pub ecus: Vec<Ecu>,
    pub ecu_types: Vec<EcuType>,
    pub commands: Vec<Command>,
    pub protocols: Vec<Protocol>,
    /// (type de calculateur, code défaut en hexadécimal majuscule) → libellés bruts distincts de `FAULT.FAULIB`.
    defauts: BTreeMap<(i64, String), Vec<String>>,
    /// La base reste ouverte pour les lectures à la demande ([`Gpc::parametres`]).
    db: Database,
}

/// Paramètre d'un écran de Lexia (`PARAM`) : l'habillage d'une mesure ou d'un champ d'identification.
/// Les textes sont bruts, à résoudre avec [`crate::libelles`].
#[derive(Debug, Clone, PartialEq)]
pub struct Parametre {
    /// Nom interne, celui des fiches des modules `CAL<n>` (ex. `REGIMEMOTEUR`).
    pub nom: String,
    pub libelle: String,
    pub unite: String,
    /// Format d'affichage `chiffres.décimales`, ex. `1.2` ; vide si absent.
    pub format: String,
    /// Valeurs discrètes : (valeur, texte brut), ex. (1, « Boucle fermée »).
    pub valeurs: Vec<(f64, String)>,
}

/// Identification attendue d'un calculateur (`RECO`), pour reconnaître son type au test global.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconnaissance {
    pub ecu_id: i64,
    /// Trame d'identification attendue (`61 80 …` sur 24 octets) ; un octet nul est un joker.
    pub trame: Vec<u8>,
    /// Variante (ex. moteur `MO6FZ`), vide si absente.
    pub port: String,
}

impl Reconnaissance {
    /// Vrai si la réponse d'identification (KWP, `61 80 …`) correspond : chaque octet non nul de la trame attendue
    /// est égal à celui de la réponse. [C] La C5 du 2026-11-13 (références 9644674580 et 9655212680) correspond à
    /// une ligne du type 356 (et du 389), les octets du fournisseur et de `61 80` étant des jokers.
    pub fn correspond(&self, ident: &[u8]) -> bool {
        self.trame.iter().any(|&b| b != 0)
            && self.trame.iter().enumerate().all(|(i, &b)| b == 0 || ident.get(i) == Some(&b))
    }
}

fn int(r: &Row, name: &str) -> Option<i64> {
    col(r, name).and_then(Value::as_i64)
}

fn req(r: &Row, table: &str, name: &str) -> Result<i64> {
    int(r, name).ok_or_else(|| Error::Format(format!("{table}.{name} absent")))
}

fn text(r: &Row, name: &str) -> String {
    col(r, name).and_then(Value::as_str).unwrap_or("").to_string()
}

fn bytes(r: &Row, name: &str) -> Vec<u8> {
    col(r, name).and_then(Value::as_bytes).unwrap_or(&[]).to_vec()
}

impl Gpc {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_db(Database::open(path)?)
    }

    pub fn from_db(db: Database) -> Result<Self> {
        let vehicules = db
            .rows("VEHICULE", false)?
            .iter()
            .map(|r| {
                Ok(Vehicule {
                    id: req(r, "VEHICULE", "VEHID")?,
                    com_type: text(r, "VEHCOMTYPE"),
                    mux: int(r, "VEHMUX") == Some(1),
                })
            })
            .collect::<Result<_>>()?;

        let famtypes: Vec<(i64, String, bool)> = db
            .rows("FAMTYPE", false)?
            .iter()
            .map(|r| {
                Ok((
                    req(r, "FAMTYPE", "FAMTYID")?,
                    text(r, "FAMNAME"),
                    int(r, "FAMGATEWAY") == Some(1),
                ))
            })
            .collect::<Result<_>>()?;
        let families = db
            .rows("FAMILY", false)?
            .iter()
            .map(|r| {
                let ty = famtypes.iter().find(|(i, _, _)| Some(*i) == int(r, "FAMTYID"));
                Ok(Family {
                    id: req(r, "FAMILY", "FAMID")?,
                    veh_id: req(r, "FAMILY", "VEHID")?,
                    name: ty.map(|(_, n, _)| n.clone()).unwrap_or_default(),
                    target: text(r, "FAMCODETARGET"),
                    mux: int(r, "FAMMUX") == Some(1),
                    passerelle: ty.is_some_and(|(_, _, g)| *g),
                })
            })
            .collect::<Result<_>>()?;

        let ecus: Vec<Ecu> = db
            .rows("ECU", false)?
            .iter()
            .map(|r| {
                Ok(Ecu {
                    id: req(r, "ECU", "ECUID")?,
                    ecu_type_id: req(r, "ECU", "ECUTYID")?,
                    fam_id: req(r, "ECU", "FAMID")?,
                    in_global_test: int(r, "ECUINGLOBTST") == Some(1),
                })
            })
            .collect::<Result<_>>()?;

        let ecu_types = db
            .rows("ECUTYPE", false)?
            .iter()
            .map(|r| {
                Ok(EcuType {
                    id: req(r, "ECUTYPE", "ECUTYID")?,
                    name: text(r, "ECUNAME"),
                    marque: text(r, "ECUMARQUE"),
                    comment: text(r, "ECUCOMMENT"),
                    prot_id: int(r, "PROTID"),
                    equ_fname: text(r, "ECUEQUFNAME"),
                })
            })
            .collect::<Result<_>>()?;

        let mut commands = Vec::new();
        for r in &db.rows("COMMAND", true)? {
            let (cal, com) = (bytes(r, "COMECU"), bytes(r, "COMCODE"));
            let (&[code_cal], &[code_com]) = (cal.as_slice(), com.as_slice()) else {
                return Err(Error::Format("COMMAND : COMECU / COMCODE ne font pas un octet".into()));
            };
            commands.push(Command {
                id: req(r, "COMMAND", "COMID")?,
                ecu_type_id: req(r, "COMMAND", "ECUTYID")?,
                name: text(r, "COMNAME"),
                code_cal,
                code_com,
                data: bytes(r, "COMDATA"),
                data_type: text(r, "COMDATATYPE"),
            });
        }
        commands.sort_by_key(|c| c.id);

        let protocols = db
            .rows("PROTOCOL", true)?
            .iter()
            .map(|r| {
                let files = (1..=5)
                    .filter_map(|i| {
                        let nom = text(r, &format!("PROTFNAME{i}"));
                        let b = col(r, &format!("PROTFILE{i}")).and_then(Value::as_bytes)?;
                        (!nom.is_empty()).then(|| (nom, b.to_vec()))
                    })
                    .collect();
                Ok(Protocol {
                    id: req(r, "PROTOCOL", "PROTID")?,
                    type_id: int(r, "PROTTYID"),
                    files,
                })
            })
            .collect::<Result<_>>()?;

        // défauts connus : I_ECUFAU relie un calculateur (ECUID) à des lignes de FAULT (code, libellé)
        let type_de: HashMap<i64, i64> = ecus.iter().map(|e| (e.id, e.ecu_type_id)).collect();
        let mut fault: HashMap<i64, (String, String)> = HashMap::new();
        for r in &db.rows("FAULT", false)? {
            let (code, lib) = (text(r, "FAUCODEECU").to_ascii_uppercase(), text(r, "FAULIB"));
            if !code.is_empty() && !lib.is_empty() {
                fault.insert(req(r, "FAULT", "FAUID")?, (code, lib));
            }
        }
        let mut defauts: BTreeMap<(i64, String), Vec<String>> = BTreeMap::new();
        for r in &db.rows("I_ECUFAU", false)? {
            let (Some(ecu), Some(fau)) = (int(r, "ECUID"), int(r, "FAUID")) else {
                continue;
            };
            let (Some(&ty), Some((code, lib))) = (type_de.get(&ecu), fault.get(&fau)) else {
                continue;
            };
            let libelles = defauts.entry((ty, code.clone())).or_default();
            if !libelles.contains(lib) {
                libelles.push(lib.clone());
            }
        }

        Ok(Self {
            vehicules,
            families,
            ecus,
            ecu_types,
            commands,
            protocols,
            defauts,
            db,
        })
    }

    /// Paramètres des écrans d'un type de calculateur (libellé, unité, format, valeurs discrètes), par nom.
    /// Lecture à la demande : `SCREEN` → `I_SCRPAR` → `PARAM` → `I_PARDIS` → `DISCVAL`. Quand la base a
    /// plusieurs variantes d'un même nom (`PARIDCONF`), la première (configuration la plus basse) est gardée.
    pub fn parametres(&self, ecu_type_id: i64) -> Result<Vec<Parametre>> {
        let texte = |r: &Row, name: &str| match col(r, name) {
            Some(Value::Text(t)) => t.clone(),
            Some(Value::Bytes(b)) => crate::text::decode(b, crate::text::Codepage::Cp1252),
            _ => String::new(),
        };
        let ecus: HashSet<i64> = self
            .ecus
            .iter()
            .filter(|e| e.ecu_type_id == ecu_type_id)
            .map(|e| e.id)
            .collect();
        let ecrans: HashSet<i64> = self
            .db
            .rows_cle("SCREEN", false, "ECUID", &ecus)?
            .iter()
            .filter_map(|r| int(r, "SCRID"))
            .collect();
        let ids: HashSet<i64> = self
            .db
            .rows_cle("I_SCRPAR", false, "SCRID", &ecrans)?
            .iter()
            .filter_map(|r| int(r, "PARID"))
            .collect();
        // une ligne par nom : la configuration la plus basse
        let mut par_nom: BTreeMap<String, (i64, i64, Parametre)> = BTreeMap::new();
        for r in &self.db.rows_cle("PARAM", true, "PARID", &ids)? {
            let (Some(id), nom) = (int(r, "PARID"), text(r, "PARNAME")) else {
                continue;
            };
            let conf = int(r, "PARIDCONF").unwrap_or(0);
            if nom.is_empty() || par_nom.get(&nom).is_some_and(|(c, _, _)| *c <= conf) {
                continue;
            }
            let p = Parametre {
                nom: nom.clone(),
                libelle: texte(r, "PARLIBMP"),
                unite: texte(r, "PARLIBUNIT"),
                format: text(r, "PARFORMAT"),
                valeurs: Vec::new(),
            };
            par_nom.insert(nom, (conf, id, p));
        }
        let gardes: HashSet<i64> = par_nom.values().map(|(_, id, _)| *id).collect();
        let liens: Vec<(i64, i64)> = self
            .db
            .rows_cle("I_PARDIS", false, "PARID", &gardes)?
            .iter()
            .filter_map(|r| Some((int(r, "PARID")?, int(r, "DISCID")?)))
            .collect();
        let disc: HashSet<i64> = liens.iter().map(|(_, d)| *d).collect();
        let valeurs: HashMap<i64, (f64, String)> = self
            .db
            .rows_cle("DISCVAL", true, "DISCID", &disc)?
            .iter()
            .filter_map(|r| {
                let v = col(r, "DISCVALCOM").and_then(Value::as_f64)?;
                Some((int(r, "DISCID")?, (v, texte(r, "DISCVALLIB"))))
            })
            .collect();
        Ok(par_nom
            .into_values()
            .map(|(_, id, mut p)| {
                p.valeurs = liens
                    .iter()
                    .filter(|(pid, _)| *pid == id)
                    .filter_map(|(_, d)| valeurs.get(d).cloned())
                    .collect();
                p.valeurs.sort_by(|a, b| a.0.total_cmp(&b.0));
                p
            })
            .collect())
    }

    /// Libellés bruts d'un code défaut (hexadécimal, ex. `0420`) pour un type de calculateur ; à résoudre
    /// avec [`crate::libelles`]. Plusieurs libellés si la base en donne selon le véhicule.
    pub fn libelles_defaut(&self, ecu_type_id: i64, code: &str) -> &[String] {
        self.defauts
            .get(&(ecu_type_id, code.to_ascii_uppercase()))
            .map_or(&[], Vec::as_slice)
    }

    /// Nombre de couples (type de calculateur, code défaut) connus.
    pub fn nb_defauts(&self) -> usize {
        self.defauts.len()
    }

    pub fn ecu_type(&self, id: i64) -> Option<&EcuType> {
        self.ecu_types.iter().find(|e| e.id == id)
    }

    /// Commandes d'un type de calculateur, dans l'ordre de la base.
    pub fn commands_of(&self, ecu_type_id: i64) -> impl Iterator<Item = &Command> {
        self.commands.iter().filter(move |c| c.ecu_type_id == ecu_type_id)
    }

    pub fn command(&self, ecu_type_id: i64, name: &str) -> Option<&Command> {
        self.commands_of(ecu_type_id).find(|c| c.name == name)
    }

    pub fn protocol(&self, id: i64) -> Option<&Protocol> {
        self.protocols.iter().find(|p| p.id == id)
    }

    /// Protocole d'un type de calculateur.
    pub fn protocol_of(&self, ecu_type_id: i64) -> Option<&Protocol> {
        self.protocol(self.ecu_type(ecu_type_id)?.prot_id?)
    }

    pub fn vehicule(&self, com_type: &str) -> Option<&Vehicule> {
        self.vehicules
            .iter()
            .find(|v| v.com_type.eq_ignore_ascii_case(com_type))
    }

    /// Identifications attendues des calculateurs donnés (`RECO`, lue à la demande) : par calculateur (`ECUID`),
    /// trame de `RECTRAMEIDF` et nom de `RECPORTNAME` (ex. moteur `MO6FZ`).
    pub fn reconnaissances(&self, ecu_ids: &HashSet<i64>) -> Result<Vec<Reconnaissance>> {
        Ok(self
            .db
            .rows_cle("RECO", false, "ECUID", ecu_ids)?
            .iter()
            .filter_map(|r| {
                let h = text(r, "RECTRAMEIDF");
                let trame = (0..h.len())
                    .step_by(2)
                    .map(|i| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok()))
                    .collect::<Option<Vec<u8>>>()?;
                let ecu_id = int(r, "ECUID")?;
                (!trame.is_empty()).then(|| Reconnaissance {
                    ecu_id,
                    trame,
                    port: text(r, "RECPORTNAME"),
                })
            })
            .collect())
    }

    /// Calculateurs possibles d'un véhicule : (famille, calculateur, type).
    pub fn ecus_of_vehicule(&self, veh_id: i64) -> Vec<(&Family, &Ecu, &EcuType)> {
        let mut out = Vec::new();
        for f in self.families.iter().filter(|f| f.veh_id == veh_id) {
            for e in self.ecus.iter().filter(|e| e.fam_id == f.id) {
                if let Some(t) = self.ecu_type(e.ecu_type_id) {
                    out.push((f, e, t));
                }
            }
        }
        out
    }

    /// Type de calculateur passerelle (famille `FAMGATEWAY = 1`, la BSI) par lequel Lexia joint un calculateur
    /// du même canal pendant le test global : constaté le 2026-11-13 sur la C5 pour le combiné, la
    /// climatisation, l'afficheur et le sac gonflable (canal `7a`, session ouverte avec la BSI). Pour chaque
    /// véhicule où le type existe (seulement `vehicule` s'il est donné), le premier calculateur de la famille
    /// passerelle sur le même canal ; `None` si le type est lui-même dans une famille passerelle, s'il n'y a pas
    /// de passerelle sur son canal, ou si les véhicules ne s'accordent pas.
    pub fn passerelle(&self, ecu_type_id: i64, vehicule: Option<&str>) -> Option<&EcuType> {
        let canal = |id: i64| self.commands_of(id).next().map(|c| c.code_cal);
        let code_cal = canal(ecu_type_id)?;
        let veh_id = match vehicule {
            Some(v) => Some(self.vehicule(v)?.id),
            None => None,
        };
        let familles: Vec<&Family> = self
            .ecus
            .iter()
            .filter(|e| e.ecu_type_id == ecu_type_id)
            .filter_map(|e| self.families.iter().find(|f| f.id == e.fam_id))
            .filter(|f| veh_id.is_none_or(|v| f.veh_id == v))
            .collect();
        if familles.is_empty() || familles.iter().any(|f| f.passerelle) {
            return None;
        }
        let mut choix: Option<i64> = None;
        let mut vus = Vec::new();
        for f in familles {
            if vus.contains(&f.veh_id) {
                continue;
            }
            vus.push(f.veh_id);
            let p = self
                .ecus_of_vehicule(f.veh_id)
                .into_iter()
                .find(|(fp, _, t)| fp.passerelle && canal(t.id) == Some(code_cal))
                .map(|(_, _, t)| t.id)?;
            if choix.is_some_and(|c| c != p) {
                return None;
            }
            choix = Some(p);
        }
        self.ecu_type(choix?)
    }
}
