//! Transport de rejeu : répond avec les échanges d'une capture (`echanges_*.jsonl`), sans matériel.
//!
//! Une ligne JSON par échange : `code_cal`, `code_com`, `octet4`, `etat`, `req`, `rep` (hexadécimal), `heure`.

use crate::{Request, Response, Transport, VciError};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Echange {
    /// Heure de la requête (`HH:MM:SS.mmm`).
    pub heure: String,
    pub req: Request,
    pub rep: Response,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// La requête doit être exactement la suivante de la capture.
    Strict,
    /// La première requête identique à partir de la position courante.
    Recherche,
    /// Comme `Recherche`, mais reprend au début de la capture une fois la fin atteinte (démonstration).
    Boucle,
}

pub struct Replay {
    echanges: Vec<Echange>,
    pos: usize,
    pub mode: Mode,
}

fn hex(s: &str) -> Option<Vec<u8>> {
    if !s.is_ascii() || !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

impl Replay {
    pub fn open(path: impl AsRef<Path>, mode: Mode) -> Result<Self, VciError> {
        Self::parse(&std::fs::read_to_string(path)?, mode)
    }

    pub fn parse(jsonl: &str, mode: Mode) -> Result<Self, VciError> {
        let mut echanges = Vec::new();
        for (n, l) in jsonl.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
            let err = |quoi: &str| VciError::Rejeu(format!("ligne {} : {quoi}", n + 1));
            let v: serde_json::Value = serde_json::from_str(l).map_err(|e| err(&e.to_string()))?;
            let octets = |k: &str| v[k].as_str().and_then(hex).ok_or_else(|| err(k));
            let octet = |k: &str| match octets(k)?.as_slice() {
                &[b] => Ok(b),
                _ => Err(err(k)),
            };
            echanges.push(Echange {
                heure: v["heure"].as_str().unwrap_or("").to_string(),
                req: Request {
                    code_cal: octet("code_cal")?,
                    code_com: octet("code_com")?,
                    octet4: octet("octet4")?,
                    data: octets("req")?,
                },
                rep: Response {
                    etat: octet("etat")?,
                    data: octets("rep")?,
                },
            });
        }
        Ok(Self { echanges, pos: 0, mode })
    }

    pub fn echanges(&self) -> &[Echange] {
        &self.echanges
    }

    /// Index du prochain échange attendu.
    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn aller_a(&mut self, index: usize) {
        self.pos = index.min(self.echanges.len());
    }

    /// Place le rejeu sur le premier échange dont l'heure commence par `prefixe` ; renvoie son index.
    pub fn aller_a_heure(&mut self, prefixe: &str) -> Option<usize> {
        let i = self.echanges.iter().position(|e| e.heure.starts_with(prefixe))?;
        self.pos = i;
        Some(i)
    }
}

impl Transport for Replay {
    fn exchange(&mut self, req: &Request) -> Result<Response, VciError> {
        let i = match self.mode {
            Mode::Strict => match self.echanges.get(self.pos) {
                Some(e) if e.req == *req => self.pos,
                Some(e) => {
                    return Err(VciError::Rejeu(format!(
                        "échange n° {} ({}) : attendu « {} », reçu « {req} »",
                        self.pos, e.heure, e.req
                    )))
                }
                None => return Err(VciError::Rejeu(format!("fin de la capture, requête « {req} »"))),
            },
            Mode::Recherche | Mode::Boucle => {
                let n = self.echanges.len();
                let fin = if self.mode == Mode::Boucle { self.pos + n } else { n };
                (self.pos..fin)
                    .map(|k| k % n.max(1))
                    .find(|&k| self.echanges.get(k).is_some_and(|e| e.req == *req))
                    .ok_or_else(|| VciError::Rejeu(format!("aucun échange « {req} » après le n° {}", self.pos)))?
            }
        };
        self.pos = i + 1;
        Ok(self.echanges[i].rep.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const J: &str = r#"{"heure": "10:00:00.000", "code_cal": "00", "code_com": "fe", "octet4": "aa", "etat": "01", "req": "", "rep": "0100"}
{"heure": "10:00:01.000", "code_cal": "8e", "code_com": "07", "octet4": "aa", "etat": "01", "req": "c28001", "rep": "0110"}
{"heure": "10:00:02.000", "code_cal": "00", "code_com": "fe", "octet4": "aa", "etat": "01", "req": "", "rep": "0000"}
"#;

    #[test]
    fn strict() {
        let mut r = Replay::parse(J, Mode::Strict).unwrap();
        assert_eq!(r.exchange(&Request::new(0, 0xfe, [])).unwrap().data, [1, 0]);
        assert!(r.exchange(&Request::new(0x8e, 7, [0xc0, 0x80, 1])).is_err());
        assert_eq!(r.position(), 1);
        assert_eq!(
            r.exchange(&Request::new(0x8e, 7, [0xc2, 0x80, 1])).unwrap().data,
            [1, 0x10]
        );
        assert_eq!(r.aller_a_heure("10:00:02"), Some(2));
        assert_eq!(r.exchange(&Request::new(0, 0xfe, [])).unwrap().data, [0, 0]);
        assert!(r.exchange(&Request::new(0, 0xfe, [])).is_err());
    }

    #[test]
    fn recherche() {
        let mut r = Replay::parse(J, Mode::Recherche).unwrap();
        assert_eq!(r.exchange(&Request::new(0x8e, 7, [0xc2, 0x80, 1])).unwrap().etat, 1);
        assert_eq!(r.exchange(&Request::new(0, 0xfe, [])).unwrap().data, [0, 0]);
        assert!(Replay::parse("{\"code_cal\": \"zz\"}", Mode::Strict).is_err());
    }

    #[test]
    fn boucle() {
        let mut r = Replay::parse(J, Mode::Boucle).unwrap();
        for _ in 0..3 {
            assert_eq!(
                r.exchange(&Request::new(0x8e, 7, [0xc2, 0x80, 1])).unwrap().data,
                [1, 0x10]
            );
        }
        assert_eq!(r.exchange(&Request::new(0, 0xfe, [])).unwrap().data, [0, 0]);
        assert_eq!(r.exchange(&Request::new(0, 0xfe, [])).unwrap().data, [1, 0]);
        assert!(r.exchange(&Request::new(0x8e, 7, [0xc0, 0x80, 1])).is_err());
    }
}
