//! Base véhicule `GPC.FDB` : véhicule → famille → calculateur → type de calculateur,
//! commandes nommées (`COMMAND`) et fichiers de protocole (`PROTOCOL`).
//!
//! `COMMAND.COMECU` = Code cal (canal de la VCI), `COMMAND.COMCODE` = Code Com.
//! Un même Code Com désigne des commandes différentes selon le type de calculateur.

use crate::ods10::{col, Database, Row, Value};
use crate::{Error, Result};
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
}

fn int(r: &Row, name: &str) -> Option<i64> {
    col(r, name).and_then(Value::as_i64)
}

fn req(r: &Row, table: &str, name: &str) -> Result<i64> {
    int(r, name).ok_or_else(|| Error::Format(format!("{table}.{name} absent")))
}

fn text(r: &Row, name: &str) -> String {
    col(r, name)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn bytes(r: &Row, name: &str) -> Vec<u8> {
    col(r, name)
        .and_then(Value::as_bytes)
        .unwrap_or(&[])
        .to_vec()
}

impl Gpc {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_db(&Database::open(path)?)
    }

    pub fn from_db(db: &Database) -> Result<Self> {
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

        let famtypes: Vec<(i64, String)> = db
            .rows("FAMTYPE", false)?
            .iter()
            .map(|r| Ok((req(r, "FAMTYPE", "FAMTYID")?, text(r, "FAMNAME"))))
            .collect::<Result<_>>()?;
        let families = db
            .rows("FAMILY", false)?
            .iter()
            .map(|r| {
                let ty = int(r, "FAMTYID");
                Ok(Family {
                    id: req(r, "FAMILY", "FAMID")?,
                    veh_id: req(r, "FAMILY", "VEHID")?,
                    name: famtypes
                        .iter()
                        .find(|(i, _)| Some(*i) == ty)
                        .map(|(_, n)| n.clone())
                        .unwrap_or_default(),
                    target: text(r, "FAMCODETARGET"),
                    mux: int(r, "FAMMUX") == Some(1),
                })
            })
            .collect::<Result<_>>()?;

        let ecus = db
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
                return Err(Error::Format(
                    "COMMAND : COMECU / COMCODE ne font pas un octet".into(),
                ));
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

        Ok(Self {
            vehicules,
            families,
            ecus,
            ecu_types,
            commands,
            protocols,
        })
    }

    pub fn ecu_type(&self, id: i64) -> Option<&EcuType> {
        self.ecu_types.iter().find(|e| e.id == id)
    }

    /// Commandes d'un type de calculateur, dans l'ordre de la base.
    pub fn commands_of(&self, ecu_type_id: i64) -> impl Iterator<Item = &Command> {
        self.commands
            .iter()
            .filter(move |c| c.ecu_type_id == ecu_type_id)
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
}
