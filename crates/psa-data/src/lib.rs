//! Lecture seule des données PSA, dans la copie du pack de l'utilisateur.
//! Rien n'est embarqué dans le binaire : tout est lu au lancement.
//!
//! - [`ods10`] : bases Firebird / InterBase ODS 10, sans moteur Firebird ;
//! - [`dico`] : dictionnaires de libellés `.DBZ` / `.ZCL` ;
//! - [`gpc`] : base véhicule (calculateurs, commandes nommées, protocoles) ;
//! - [`mesures`] : tables de mesures des modules `CAL<n>` (dictionnaire TSV) ;
//! - [`descr`] : descripteurs de la VCI (`SUPER*.PC`, `*.68K`, `P_*.pro`).

pub mod descr;
pub mod dico;
pub mod gpc;
pub mod mesures;
pub mod ods10;
pub mod text;

use std::fmt;

/// Erreur de lecture d'un fichier de données.
#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// Lecture hors du fichier ou de l'enregistrement.
    Truncated {
        what: &'static str,
        offset: usize,
    },
    /// Contenu inattendu.
    Format(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "E/S : {e}"),
            Error::Truncated { what, offset } => {
                write!(f, "données tronquées ({what}, offset {offset})")
            }
            Error::Format(m) => write!(f, "format : {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// Lectures little-endian bornées.
pub(crate) mod rd {
    use super::{Error, Result};

    fn take<const N: usize>(b: &[u8], o: usize, what: &'static str) -> Result<[u8; N]> {
        b.get(o..o + N)
            .and_then(|s| s.try_into().ok())
            .ok_or(Error::Truncated { what, offset: o })
    }
    pub fn u8(b: &[u8], o: usize, what: &'static str) -> Result<u8> {
        b.get(o)
            .copied()
            .ok_or(Error::Truncated { what, offset: o })
    }
    pub fn u16(b: &[u8], o: usize, what: &'static str) -> Result<u16> {
        Ok(u16::from_le_bytes(take(b, o, what)?))
    }
    pub fn i16(b: &[u8], o: usize, what: &'static str) -> Result<i16> {
        Ok(i16::from_le_bytes(take(b, o, what)?))
    }
    pub fn u32(b: &[u8], o: usize, what: &'static str) -> Result<u32> {
        Ok(u32::from_le_bytes(take(b, o, what)?))
    }
    pub fn i32(b: &[u8], o: usize, what: &'static str) -> Result<i32> {
        Ok(i32::from_le_bytes(take(b, o, what)?))
    }
    pub fn i64(b: &[u8], o: usize, what: &'static str) -> Result<i64> {
        Ok(i64::from_le_bytes(take(b, o, what)?))
    }
    /// Tranche tolérante : tronquée aux limites.
    pub fn slice(b: &[u8], start: usize, len: usize) -> &[u8] {
        let s = start.min(b.len());
        &b[s..s.saturating_add(len).min(b.len())]
    }
}
