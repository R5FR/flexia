//! Libellés de la base véhicule (ex. `FAULT.FAULIB`) et leur résolution dans les dictionnaires.
//!
//! Un libellé est une suite d'éléments préfixés par `@` :
//!
//! ```text
//! @P3019-CITACT   phrase n° 3019 du dictionnaire CITACT (numérotée à partir de 1)
//! @\+             séparateur de parties (organe, puis nature du défaut…)
//! @\*2            valeur qui remplace le prochain « * » de la partie en cours (« bobine * » -> « bobine 2 »)
//! @T. …           texte littéral
//! ```

use crate::dico::Dico;
use crate::gpc::Parametre;
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Element {
    /// Phrase d'un dictionnaire ; `numero` commence à 1.
    Phrase {
        dico: String,
        numero: usize,
    },
    Separateur,
    /// Valeur du prochain `*` de la partie en cours.
    Valeur(String),
    Texte(String),
}

/// Découpe un libellé brut en éléments. Ce qui n'est pas reconnu est gardé comme texte.
pub fn analyser(brut: &str) -> Vec<Element> {
    let mut morceaux = brut.split('@');
    let mut elements = Vec::new();
    // avant le premier « @ » : texte libre
    if let Some(tete) = morceaux.next().filter(|t| !t.is_empty()) {
        elements.push(Element::Texte(tete.to_string()));
    }
    for m in morceaux {
        let phrase = m
            .strip_prefix('P')
            .and_then(|r| r.split_once('-'))
            .and_then(|(n, dico)| {
                let ok = !dico.is_empty() && dico.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
                n.parse().ok().filter(|_| ok).map(|numero| Element::Phrase {
                    dico: dico.to_string(),
                    numero,
                })
            });
        elements.push(if let Some(p) = phrase {
            p
        } else if m == "\\+" {
            Element::Separateur
        } else if let Some(v) = m.strip_prefix("\\*") {
            Element::Valeur(v.to_string())
        } else if let Some(t) = m.strip_prefix('T') {
            Element::Texte(t.to_string())
        } else {
            Element::Texte(m.to_string())
        });
    }
    elements
}

/// Libellé résolu : une ou plusieurs parties (la première désigne en général l'organe).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Libelle {
    pub parties: Vec<String>,
}

impl fmt::Display for Libelle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.parties.split_first() {
            None => Ok(()),
            Some((tete, [])) => write!(f, "{tete}"),
            Some((tete, reste)) => write!(f, "{tete} : {}", reste.join(", ")),
        }
    }
}

/// Résout un libellé brut ; `phrase(dico, numero)` rend le texte d'une phrase (numéro à partir de 1).
/// Une phrase introuvable reste visible sous la forme `[P<n>-<DICO>]`.
pub fn resoudre(brut: &str, mut phrase: impl FnMut(&str, usize) -> Option<String>) -> Libelle {
    let mut parties = Vec::new();
    let mut courante = String::new();
    let mut apres_phrase = false;
    for e in analyser(brut) {
        match e {
            Element::Phrase { dico, numero } => {
                if apres_phrase {
                    courante.push(' ');
                }
                courante.push_str(&phrase(&dico, numero).unwrap_or_else(|| format!("[P{numero}-{dico}]")));
                apres_phrase = true;
                continue;
            }
            Element::Separateur => parties.push(std::mem::take(&mut courante)),
            Element::Valeur(v) => match courante.find('*') {
                Some(i) => courante.replace_range(i..=i, &v),
                None if v.is_empty() => {}
                None => {
                    courante.push(' ');
                    courante.push_str(&v);
                }
            },
            Element::Texte(t) => courante.push_str(&t),
        }
        apres_phrase = false;
    }
    parties.push(courante);
    Libelle {
        parties: parties
            .into_iter()
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect(),
    }
}

/// Habillage d'un paramètre, textes résolus : ce que Lexia affiche pour une mesure ou un champ.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Habillage {
    pub libelle: String,
    pub unite: String,
    /// Nombre de décimales du format d'affichage, s'il est donné.
    pub decimales: Option<usize>,
    /// Valeurs discrètes : (valeur, texte), ex. (1, « Boucle fermée »).
    pub valeurs: Vec<(f64, String)>,
}

impl Habillage {
    /// Texte de l'état correspondant à `v`, pour un paramètre discret.
    pub fn etat(&self, v: f64) -> Option<&str> {
        self.valeurs
            .iter()
            .find(|(x, _)| (x - v).abs() < 1e-6)
            .map(|(_, t)| t.as_str())
    }

    /// Valeur telle qu'elle s'affiche : texte de l'état, sinon nombre au format du paramètre suivi de l'unité.
    pub fn formater(&self, v: f64) -> String {
        if let Some(e) = self.etat(v) {
            return e.to_string();
        }
        let nombre = match self.decimales {
            Some(d) => format!("{v:.d$}"),
            None => format!("{}", (v * 1e4).round() / 1e4),
        };
        if self.unite.is_empty() {
            nombre
        } else {
            format!("{nombre} {}", self.unite)
        }
    }
}

/// Dictionnaires d'une langue, chargés à la demande depuis le dossier `DICO` de Lexia.
pub struct Dicos {
    dossier: PathBuf,
    langue: String,
    cache: Mutex<HashMap<String, Option<Arc<Dico>>>>,
}

impl Dicos {
    /// `dossier` : `…/APP/LEXIA/DICO` ; `langue` : code Lexia, ex. `FR`.
    pub fn new(dossier: impl Into<PathBuf>, langue: &str) -> Self {
        Self {
            dossier: dossier.into(),
            langue: langue.to_string(),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Fichier `<nom><langue>.dbz|zcl` (casse ignorée), dans le dossier puis dans ses sous-dossiers directs.
    fn fichier(&self, nom: &str) -> Option<PathBuf> {
        let cible = format!("{nom}{}", self.langue).to_lowercase();
        let convient = |p: &Path| {
            let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase());
            matches!(ext.as_deref(), Some("dbz" | "zcl"))
                && p.file_stem()
                    .is_some_and(|s| s.to_string_lossy().to_lowercase() == cible)
        };
        let entrees: Vec<PathBuf> = std::fs::read_dir(&self.dossier)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .collect();
        if let Some(p) = entrees.iter().find(|p| p.is_file() && convient(p)) {
            return Some(p.clone());
        }
        entrees
            .iter()
            .filter(|p| p.is_dir())
            .flat_map(|d| std::fs::read_dir(d).into_iter().flatten().flatten().map(|e| e.path()))
            .find(|p| p.is_file() && convient(p))
    }

    fn dico(&self, nom: &str) -> Option<Arc<Dico>> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache
            .entry(nom.to_string())
            .or_insert_with(|| self.fichier(nom).and_then(|p| Dico::open(p).ok()).map(Arc::new))
            .clone()
    }

    /// Phrase n° `numero` (à partir de 1) du dictionnaire `nom`.
    pub fn phrase(&self, nom: &str, numero: usize) -> Option<String> {
        self.dico(nom)?.get(numero.checked_sub(1)?)
    }

    pub fn libelle(&self, brut: &str) -> Libelle {
        resoudre(brut, |d, n| self.phrase(d, n))
    }

    /// Habillage résolu d'un paramètre de la base.
    pub fn habiller(&self, p: &Parametre) -> Habillage {
        Habillage {
            libelle: self.libelle(&p.libelle).to_string(),
            unite: self.libelle(&p.unite).to_string(),
            decimales: p.format.split_once('.').and_then(|(_, d)| d.parse().ok()),
            valeurs: p
                .valeurs
                .iter()
                .map(|(v, t)| (*v, self.libelle(t).to_string()))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phrase(d: &str, n: usize) -> Option<String> {
        match (d, n) {
            ("DIC", 1) => Some("Organe".into()),
            ("DIC", 2) => Some("Rupture".into()),
            ("DIC", 3) => Some("Bobine *".into()),
            ("DIC", 4) => Some("signaux * et *".into()),
            _ => None,
        }
    }

    #[test]
    fn analyse() {
        assert_eq!(
            analyser("@P1-DIC@\\*2@\\+@T. @P22-A_B"),
            [
                Element::Phrase {
                    dico: "DIC".into(),
                    numero: 1
                },
                Element::Valeur("2".into()),
                Element::Separateur,
                Element::Texte(". ".into()),
                Element::Phrase {
                    dico: "A_B".into(),
                    numero: 22
                },
            ]
        );
        assert_eq!(
            analyser("libre@Px-DIC"),
            [Element::Texte("libre".into()), Element::Texte("Px-DIC".into())]
        );
        assert!(analyser("").is_empty());
    }

    #[test]
    fn habillage() {
        let h = Habillage {
            unite: "V".into(),
            decimales: Some(2),
            ..Default::default()
        };
        assert_eq!(h.formater(14.2), "14.20 V");
        let h = Habillage {
            valeurs: vec![(0.0, "Non".into()), (1.0, "oui".into())],
            ..Default::default()
        };
        assert_eq!((h.formater(1.0), h.formater(3.0)), ("oui".to_string(), "3".to_string()));
        assert_eq!(Habillage::default().formater(0.64349999), "0.6435");
    }

    #[test]
    fn resolution() {
        assert_eq!(resoudre("@P1-DIC@\\+@P2-DIC", phrase).to_string(), "Organe : Rupture");
        assert_eq!(resoudre("@P1-DIC", phrase).parties, ["Organe"]);
        assert_eq!(
            resoudre("@P3-DIC@\\*2@\\+@P2-DIC", phrase).parties,
            ["Bobine 2", "Rupture"]
        );
        assert_eq!(resoudre("@P4-DIC@\\*1@\\*2", phrase).parties, ["signaux 1 et 2"]);
        assert_eq!(resoudre("@P1-DIC@\\*4", phrase).parties, ["Organe 4"]);
        assert_eq!(
            resoudre("@P1-DIC@T, @P2-DIC@\\+@T@\\+@P9-DIC", phrase).parties,
            ["Organe, Rupture", "[P9-DIC]"]
        );
        assert_eq!(resoudre("@P1-DIC@P2-DIC", phrase).parties, ["Organe Rupture"]);
        assert!(resoudre("", phrase).parties.is_empty());
    }
}
