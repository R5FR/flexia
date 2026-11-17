//! Transport vers l'interface de diagnostic (VCI ACTIA « PP2000 »).
//!
//! Un échange = un message applicatif envoyé, une réponse reçue :
//!
//! ```text
//! requête : Code cal | Code Com | octet « dernier bloc » | données (≤ 8 012 o)
//! réponse : état | données
//! ```
//!
//! La couche liaison USB (trames `40…44`, sommes de contrôle, fragments) est faite par le pilote.

#[cfg(windows)]
pub mod pp2000;
pub mod replay;

use std::fmt;

/// Taille maximale des données d'un message ; au-delà, plusieurs messages chaînés.
pub const BLOC_MAX: usize = 8012;
/// Octet 4 : dernier (ou seul) message.
pub const DERNIER: u8 = 0xAA;
/// Octet 4 : un autre message suit.
pub const SUITE: u8 = 0x55;
/// Code cal des commandes système de la VCI.
pub const CAL_SYSTEME: u8 = 0x00;

/// États de réponse constatés.
pub mod etat {
    /// Réponse du calculateur (ou de la VCI) présente.
    pub const REPONSE: u8 = 0x01;
    /// Commande système acceptée.
    pub const ACCEPTE: u8 = 0x03;
    /// Vu une fois en réponse à un RESET.
    pub const RESET: u8 = 0x0C;
    /// Pas de réponse du calculateur.
    pub const SANS_REPONSE: u8 = 0x81;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub code_cal: u8,
    pub code_com: u8,
    /// [`DERNIER`] ou [`SUITE`].
    pub octet4: u8,
    pub data: Vec<u8>,
}

impl Request {
    pub fn new(code_cal: u8, code_com: u8, data: impl Into<Vec<u8>>) -> Self {
        Self {
            code_cal,
            code_com,
            octet4: DERNIER,
            data: data.into(),
        }
    }

    /// Messages chaînés pour des données de taille quelconque (fichiers de protocole).
    pub fn blocs(code_cal: u8, code_com: u8, data: &[u8]) -> Vec<Self> {
        if data.is_empty() {
            return vec![Self::new(code_cal, code_com, Vec::new())];
        }
        let n = data.len().div_ceil(BLOC_MAX);
        data.chunks(BLOC_MAX)
            .enumerate()
            .map(|(i, c)| Self {
                code_cal,
                code_com,
                octet4: if i + 1 < n { SUITE } else { DERNIER },
                data: c.to_vec(),
            })
            .collect()
    }
}

impl fmt::Display for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:02x}/{:02x} {:02x} [{} o]",
            self.code_cal,
            self.code_com,
            self.octet4,
            self.data.len()
        )?;
        for b in self.data.iter().take(16) {
            write!(f, " {b:02x}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub etat: u8,
    pub data: Vec<u8>,
}

#[derive(Debug)]
pub enum VciError {
    Io(std::io::Error),
    /// Le rejeu ne contient pas cet échange à cet endroit.
    Rejeu(String),
    /// Autre erreur de transport.
    Transport(String),
}

impl fmt::Display for VciError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VciError::Io(e) => write!(f, "E/S : {e}"),
            VciError::Rejeu(m) => write!(f, "rejeu : {m}"),
            VciError::Transport(m) => write!(f, "transport : {m}"),
        }
    }
}

impl std::error::Error for VciError {}

impl From<std::io::Error> for VciError {
    fn from(e: std::io::Error) -> Self {
        VciError::Io(e)
    }
}

/// Un échange avec la VCI.
pub trait Transport {
    fn exchange(&mut self, req: &Request) -> Result<Response, VciError>;

    /// Temporisations des commandes d'un canal : (Code Com, TEMPO) de la table `.PC` du protocole.
    /// Elles ne servent qu'à borner l'attente côté PC ; un transport sans attente les ignore.
    fn definir_tempos(&mut self, _code_cal: u8, _tempos: &[(u8, u32)]) {}
}

/// Remplace par des `X` les 10 caractères du milieu de tout numéro de série véhicule (VIN) présent en ASCII :
/// 17 caractères de l'alphabet VIN commençant par `VF`. Les 3 premiers et les 4 derniers restent lisibles.
pub fn masquer_vin(octets: &[u8]) -> Vec<u8> {
    let vin = |c: u8| c.is_ascii_digit() || (c.is_ascii_uppercase() && !matches!(c, b'I' | b'O' | b'Q'));
    let mut sortie = octets.to_vec();
    let mut i = 0;
    while i + 17 <= sortie.len() {
        if sortie[i..i + 2] == *b"VF" && sortie[i..i + 17].iter().all(|&c| vin(c)) {
            sortie[i + 3..i + 13].fill(b'X');
            i += 17;
        } else {
            i += 1;
        }
    }
    sortie
}

/// Transport qui écrit chaque échange sur la sortie d'erreur (VIN masqué), puis le délègue.
pub struct Trace<T>(pub T);

impl<T: Transport> Transport for Trace<T> {
    fn exchange(&mut self, req: &Request) -> Result<Response, VciError> {
        let debut = std::time::Instant::now();
        let r = self.0.exchange(req);
        let ms = debut.elapsed().as_millis();
        match &r {
            Ok(rep) => {
                let octets: String = masquer_vin(&rep.data)
                    .iter()
                    .take(48)
                    .map(|b| format!(" {b:02x}"))
                    .collect();
                eprintln!(
                    "> {req}\n< état {:02x} [{} o]{octets}  ({ms} ms)",
                    rep.etat,
                    rep.data.len()
                );
            }
            Err(e) => eprintln!("> {req}\n< ERREUR {e}  ({ms} ms)"),
        }
        r
    }

    fn definir_tempos(&mut self, code_cal: u8, tempos: &[(u8, u32)]) {
        self.0.definir_tempos(code_cal, tempos);
    }
}

impl<T: Transport + ?Sized> Transport for &mut T {
    fn exchange(&mut self, req: &Request) -> Result<Response, VciError> {
        (**self).exchange(req)
    }

    fn definir_tempos(&mut self, code_cal: u8, tempos: &[(u8, u32)]) {
        (**self).definir_tempos(code_cal, tempos);
    }
}

impl<T: Transport + ?Sized> Transport for Box<T> {
    fn exchange(&mut self, req: &Request) -> Result<Response, VciError> {
        (**self).exchange(req)
    }

    fn definir_tempos(&mut self, code_cal: u8, tempos: &[(u8, u32)]) {
        (**self).definir_tempos(code_cal, tempos);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vin_masque() {
        assert_eq!(
            masquer_vin(b"\x61\xb0VF7AB123CD4567890\x00"),
            b"\x61\xb0VF7XXXXXXXXXX7890\x00"
        );
        assert_eq!(
            masquer_vin(b"VF7AB123CD456789"),
            b"VF7AB123CD456789",
            "16 caractères : pas un VIN"
        );
        assert_eq!(
            masquer_vin(b"VF7AB123CD45O7890"),
            b"VF7AB123CD45O7890",
            "O n'est pas dans l'alphabet VIN"
        );
        assert_eq!(masquer_vin(b"9644674580"), b"9644674580");
        assert!(masquer_vin(b"").is_empty());
    }

    #[test]
    fn blocs() {
        let b = Request::blocs(0, 4, &vec![7u8; BLOC_MAX + 10]);
        assert_eq!(
            b.iter().map(|r| (r.octet4, r.data.len())).collect::<Vec<_>>(),
            [(SUITE, BLOC_MAX), (DERNIER, 10)]
        );
        assert_eq!(Request::blocs(0, 4, &vec![7u8; BLOC_MAX]).len(), 1);
        assert_eq!(Request::blocs(0, 0xfe, &[]), [Request::new(0, 0xfe, [])]);
    }
}
