//! BSI (boîtier de servitude intelligent ; type 264 « BSI générique sur X4 » de la base) : zones de données
//! (LID) dont la composition est téléchargée depuis la BSI, paramètres et défauts de son module `CAL264`.
//!
//! Constaté (détail et preuves : `work/11_plan_rust.md` §10, du 2026-11-17) :
//! - à l'ouverture, Lexia télécharge pour chaque famille de zones (`LISTEINI.INI`, `[BSI] FamillesLID =
//!   C0,B0,D0,A0,83`) leur composition : commande `MAPPING` (service `35`, données `famille 00 00 bloc`),
//!   réponses `75 <famille> <80 suite | ff fin> <bloc u24> <n> <n octets> <CRC 2 o>`. Concaténés, les blocs
//!   donnent des entrées `<id> <rang>… ff` : la zone `famille + id` contient ces paramètres, dans cet ordre
//!   (COMMVEH32 `TableLID::GetECUFrame`) ; `fb fc fd fe` = bourrage double mot, mot, octet, bit ; un rang
//!   répété dans une entrée devient un bourrage de son type ;
//! - type de chaque rang : `BSI.ini` (`[LID]`, `rang=B|W|D|b|0xNN`). Positions à la suite ; les bits
//!   consécutifs partagent un octet, du poids faible au poids fort, 8 au plus (`RemplirMappingAndMasque`) ; un
//!   bit vaut `octet & masque` (`GetParamValue`). Vérifié sur les zones b1, b2, 84 et 87 du 2026-11-13 ;
//! - paramètres : dictionnaire TSV de `scripts/bsi_conversions.py` (entrées de CAL264, conversions obtenues en
//!   exécutant le module) ; défauts : `scripts/bsi_defauts.py` (sa fonction `LECTDEF3020`, bit par bit).

use crate::{Error, Result};
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::path::Path;

/// Type de la BSI générique sur X4 dans la base.
pub const TYPE_BSI_X4: i64 = 264;
/// Familles de zones dont Lexia télécharge la composition (`LISTEINI.INI`, section `[BSI]`).
pub const FAMILLES: [u8; 5] = [0xC0, 0xB0, 0xD0, 0xA0, 0x83];
/// Zones de défauts lues par CAL264 (`LECTDEF3020`), dans son ordre : paires (présents, mémorisés).
pub const ZONES_DEFAUTS: [u8; 4] = [0x83, 0x84, 0x88, 0x87];
const PAIRES_DEFAUTS: [(u8, u8); 2] = [(0x83, 0x84), (0x88, 0x87)];

/// Type d'un paramètre de zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeParam {
    Octet,
    Mot,
    DoubleMot,
    Bit,
    /// Chaîne de n octets (`0xNN` dans `BSI.ini`).
    Chaine(usize),
}

impl TypeParam {
    fn depuis_ini(v: &str) -> Option<Self> {
        match v.trim() {
            "B" => Some(TypeParam::Octet),
            "W" => Some(TypeParam::Mot),
            "D" => Some(TypeParam::DoubleMot),
            "b" => Some(TypeParam::Bit),
            s => s
                .strip_prefix("0x")
                .or_else(|| s.strip_prefix("0X"))
                .and_then(|h| usize::from_str_radix(h, 16).ok())
                .filter(|&n| n > 0)
                .map(TypeParam::Chaine),
        }
    }

    fn depuis_bourrage(code: u8) -> Option<Self> {
        match code {
            0xfb => Some(TypeParam::DoubleMot),
            0xfc => Some(TypeParam::Mot),
            0xfd => Some(TypeParam::Octet),
            0xfe => Some(TypeParam::Bit),
            _ => None,
        }
    }

    /// Octets de la valeur (un bit : 1 octet valant 0 ou 1).
    pub fn taille(self) -> usize {
        match self {
            TypeParam::Octet | TypeParam::Bit => 1,
            TypeParam::Mot => 2,
            TypeParam::DoubleMot => 4,
            TypeParam::Chaine(n) => n,
        }
    }
}

/// `BSI.ini` : par zone, type de chaque rang.
#[derive(Debug, Clone, Default)]
pub struct Ini {
    zones: BTreeMap<u8, BTreeMap<u16, TypeParam>>,
}

impl Ini {
    pub fn charger(chemin: impl AsRef<Path>) -> Result<Self> {
        Ok(Self::lire(&String::from_utf8_lossy(&std::fs::read(chemin)?)))
    }

    pub fn lire(texte: &str) -> Self {
        let mut zones: BTreeMap<u8, BTreeMap<u16, TypeParam>> = BTreeMap::new();
        let mut cur = None;
        for l in texte.lines().map(str::trim) {
            if let Some(s) = l.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                cur = u8::from_str_radix(s.trim(), 16).ok();
                if let Some(z) = cur {
                    zones.entry(z).or_default();
                }
            } else if let (Some(z), Some((k, v))) = (cur, l.split_once('=')) {
                if let (Ok(rang), Some(t)) = (k.trim().parse::<u16>(), TypeParam::depuis_ini(v)) {
                    zones.entry(z).or_default().insert(rang, t);
                }
            }
        }
        Self { zones }
    }

    pub fn types(&self, lid: u8) -> Option<&BTreeMap<u16, TypeParam>> {
        self.zones.get(&lid)
    }
}

/// Familles de zones d'un calculateur dans `LISTEINI.INI` (`[BSI] FamillesLID = C0,B0,D0,A0,83`).
pub fn familles_liste_ini(texte: &str, nom: &str) -> Option<Vec<u8>> {
    let mut dans = false;
    for l in texte.lines().map(str::trim) {
        if let Some(s) = l.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            dans = s.trim().eq_ignore_ascii_case(nom);
        } else if dans {
            if let Some((k, v)) = l.split_once('=') {
                if k.trim().eq_ignore_ascii_case("FamillesLID") {
                    return Some(v.split(',').filter_map(|x| u8::from_str_radix(x.trim(), 16).ok()).collect());
                }
            }
        }
    }
    None
}

/// Paramètre placé dans une zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Champ {
    /// Rang dans `BSI.ini` ; bourrage : 0x100 + n.
    pub rang: u16,
    pub type_param: TypeParam,
    /// Octet de la zone (0 = premier octet après `61 LID`).
    pub position: usize,
    /// Masque d'un bit ; 0 pour les autres types.
    pub masque: u8,
}

impl Champ {
    /// Octets bruts du paramètre dans les données de la zone ; un bit vaut `octet & masque`.
    pub fn brut(&self, donnees: &[u8]) -> Option<Vec<u8>> {
        match self.type_param {
            TypeParam::Bit => donnees.get(self.position).map(|b| vec![b & self.masque]),
            t => donnees.get(self.position..self.position + t.taille()).map(<[u8]>::to_vec),
        }
    }
}

/// Composition des zones de la BSI : par zone, les codes de sa table (rangs, ou `fb`…`fe` pour un bourrage).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Composition {
    zones: BTreeMap<u8, Vec<u8>>,
}

impl Composition {
    /// Depuis les réponses KWP de la commande `MAPPING` (`75 …`) ; les autres réponses sont ignorées.
    pub fn depuis_mapping<'a>(reponses: impl IntoIterator<Item = &'a [u8]>) -> Self {
        let mut blocs: BTreeMap<u8, BTreeMap<u32, &[u8]>> = BTreeMap::new();
        for r in reponses {
            if r.len() < 7 || r[0] != 0x75 {
                continue;
            }
            let bloc = u32::from_be_bytes([0, r[3], r[4], r[5]]);
            if let Some(d) = r.get(7..7 + usize::from(r[6])) {
                blocs.entry(r[1]).or_default().insert(bloc, d);
            }
        }
        let mut zones = BTreeMap::new();
        for (famille, b) in blocs {
            let donnees: Vec<u8> = b.values().flat_map(|x| x.iter().copied()).collect();
            for entree in donnees.split(|&c| c == 0xff) {
                if let Some((&id, rangs)) = entree.split_first() {
                    zones.insert(famille.wrapping_add(id), rangs.to_vec());
                }
            }
        }
        Self { zones }
    }

    pub fn est_vide(&self) -> bool {
        self.zones.is_empty()
    }

    pub fn zones(&self) -> impl Iterator<Item = u8> + '_ {
        self.zones.keys().copied()
    }

    pub fn contient(&self, lid: u8) -> bool {
        self.zones.contains_key(&lid)
    }

    /// Paramètres de la zone dans l'ordre de la trame, avec leurs positions.
    pub fn champs(&self, lid: u8, ini: &Ini) -> Result<Vec<Champ>> {
        let codes = self
            .zones
            .get(&lid)
            .ok_or_else(|| Error::Format(format!("zone {lid:02x} absente de la composition de la BSI")))?;
        let vide = BTreeMap::new();
        let types = ini.types(lid).unwrap_or(&vide);
        let (mut out, mut vus, mut bourrages) = (Vec::with_capacity(codes.len()), HashSet::new(), 0u16);
        let (mut position, mut bit, mut precedent): (usize, u8, Option<TypeParam>) = (0, 0, None);
        for &code in codes {
            let (rang, t) = match TypeParam::depuis_bourrage(code) {
                Some(t) => {
                    bourrages += 1;
                    (0x100 + bourrages - 1, t)
                }
                None => {
                    let t = *types.get(&u16::from(code)).ok_or_else(|| {
                        Error::Format(format!("zone {lid:02x} : rang {code:#04x} absent de BSI.ini"))
                    })?;
                    if vus.insert(code) {
                        (u16::from(code), t)
                    } else {
                        // rang répété : bourrage de son type (GetECUFrame)
                        bourrages += 1;
                        (0x100 + bourrages - 1, t)
                    }
                }
            };
            if let Some(p) = precedent {
                if p == TypeParam::Bit {
                    if t == TypeParam::Bit && bit < 7 {
                        bit += 1;
                    } else {
                        position += 1;
                        bit = 0;
                    }
                } else {
                    position += p.taille();
                    bit = 0;
                }
            }
            let masque = if t == TypeParam::Bit { 1 << bit } else { 0 };
            out.push(Champ {
                rang,
                type_param: t,
                position,
                masque,
            });
            precedent = Some(t);
        }
        Ok(out)
    }

    /// Taille de la zone en octets.
    pub fn taille(&self, lid: u8, ini: &Ini) -> Result<usize> {
        Ok(self
            .champs(lid, ini)?
            .last()
            .map_or(0, |c| c.position + c.type_param.taille()))
    }
}

/// Fonction de conversion d'un paramètre (CAL264 seg44 : flottant 0x2167, octet 0x2851, long 0x2724…).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fonction {
    Flottant,
    Octet,
    Long,
    /// Entier décimal affiché avec un chiffre après la virgule.
    Decimal1,
    Chaine,
    Brut,
    Inconnue,
}

/// Paramètre de CAL264 (une ligne du dictionnaire `bsi_parametres_CAL264.tsv`).
#[derive(Debug, Clone, PartialEq)]
pub struct ParametreBsi {
    pub lid: u8,
    pub rang: u8,
    /// Sous-champ (0xff = paramètre entier) : premier octet, premier bit, nombre de bits.
    pub octet: u8,
    pub bit: u8,
    pub largeur: u8,
    pub nom: String,
    pub genre: u8,
    pub conversion: u8,
    pub fonction: Fonction,
    pub petit_boutiste: bool,
    pub facteur: f64,
    pub decalage: f64,
    pub masque: Option<u64>,
    pub bits: u8,
}

/// Valeur d'un paramètre de la BSI.
#[derive(Debug, Clone, PartialEq)]
pub enum Valeur {
    Nombre(f64),
    Texte(String),
    Octets(Vec<u8>),
    /// Valeur « tout à 1 » : Lexia rend 16 777 215.
    Invalide,
}

impl fmt::Display for Valeur {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Valeur::Nombre(x) => write!(f, "{}", (x * 1e4).round() / 1e4),
            Valeur::Texte(t) => write!(f, "{t}"),
            Valeur::Octets(b) => {
                let h: Vec<String> = b.iter().map(|c| format!("{c:02x}")).collect();
                write!(f, "{}", h.join(" "))
            }
            Valeur::Invalide => write!(f, "invalide"),
        }
    }
}

fn entier_be(b: &[u8]) -> u64 {
    b.iter().fold(0u64, |a, &x| (a << 8) | u64::from(x))
}

impl ParametreBsi {
    /// Valeur du paramètre dans les données d'une zone, comme le lecteur générique de CAL264 (seg44:0x0f54) :
    /// octets bruts du rang, sous-champ, puis conversion. La taille est celle du paramètre dans la BSI ; un bit
    /// vaut 0 ou 1.
    pub fn valeur(&self, champ: &Champ, donnees: &[u8]) -> Option<Valeur> {
        let mut v = champ.brut(donnees)?;
        if champ.type_param == TypeParam::Bit {
            v = vec![u8::from(v[0] != 0)];
        }
        let taille = v.len();
        self.sous_champ(&mut v);
        Some(self.convertir(&v, taille))
    }

    /// Sous-champ (seg44:0x040b) : octets à partir de `octet` ramenés en tête ; puis, à partir du bit `bit`,
    /// `largeur` bits : un seul bit donne 0 ou 1, plusieurs bits restent à leur place (masqués, non décalés).
    fn sous_champ(&self, v: &mut [u8]) {
        let taille = v.len();
        if self.octet != 0xff && self.rang != 0xff {
            let o = usize::from(self.octet).min(taille);
            for k in 0..taille - o {
                v[k] = v[o + k];
            }
        }
        if self.bit != 0xff && taille > 0 {
            let nbits = taille * 8;
            let bit = usize::from(self.bit).min(nbits - 1);
            let largeur = if self.largeur == 0xff { 1 } else { usize::from(self.largeur) };
            let (octet, debut) = (bit / 8, bit % 8);
            let fin = (debut + largeur).min(8);
            let origine = v[octet];
            let mut r = 0u8;
            for i in debut..fin {
                let m = origine & (1 << i);
                if m != 0 {
                    r = if largeur == 1 { 1 } else { r | m };
                }
            }
            v[octet] = r;
        }
    }

    fn convertir(&self, v: &[u8], taille: usize) -> Valeur {
        let n = taille.min(v.len());
        let masquer = |x: u64| self.masque.map_or(x >> self.bits, |m| (x >> self.bits) & m);
        match self.fonction {
            Fonction::Flottant => {
                let mut x = if self.petit_boutiste {
                    v[..n].iter().rev().fold(0u64, |a, &b| (a << 8) | u64::from(b))
                } else {
                    entier_be(&v[..n])
                };
                if n >= 4 {
                    x &= 0xFF_FFFF;
                }
                if (n == 1 && x == 0xFF) || (n == 2 && x == 0xFFFF) || x >= 0xFF_FFFF {
                    return Valeur::Invalide;
                }
                Valeur::Nombre(self.facteur * masquer(x) as f64 + self.decalage)
            }
            Fonction::Octet => Valeur::Nombre(v.first().map_or(0, |&b| masquer(u64::from(b))) as f64),
            Fonction::Long => {
                let n = n.min(4);
                let x = entier_be(&v[..n]);
                if n > 0 && x == (1u64 << (8 * n)) - 1 {
                    return Valeur::Invalide;
                }
                Valeur::Nombre(masquer(x) as f64)
            }
            Fonction::Decimal1 => Valeur::Nombre(entier_be(&v[..n.min(4)]) as f64 / 10.0),
            Fonction::Chaine => Valeur::Texte(
                v[..n]
                    .iter()
                    .take_while(|&&c| c != 0)
                    .map(|&c| if (0x20..0x7f).contains(&c) { c as char } else { '.' })
                    .collect(),
            ),
            Fonction::Brut | Fonction::Inconnue => Valeur::Octets(v[..n].to_vec()),
        }
    }
}

fn champ_hex(c: &str, quoi: &str, ligne: usize) -> Result<u8> {
    u8::from_str_radix(c, 16).map_err(|_| Error::Format(format!("paramètres BSI, ligne {ligne} : {quoi}")))
}

/// Dictionnaire des paramètres de la BSI.
#[derive(Debug, Clone, Default)]
pub struct DictionnaireBsi {
    parametres: Vec<ParametreBsi>,
}

impl DictionnaireBsi {
    pub fn charger(chemin: impl AsRef<Path>) -> Result<Self> {
        Self::lire(&std::fs::read_to_string(chemin)?)
    }

    pub fn lire(texte: &str) -> Result<Self> {
        let mut lignes = texte.lines();
        let attendu = [
            "lid", "rang", "octet", "bit", "largeur", "taille", "nom", "genre", "conversion", "fonction", "ordre",
            "facteur", "decalage", "masque", "bits",
        ];
        let entete: Vec<&str> = lignes.next().unwrap_or("").split('\t').collect();
        if entete != attendu {
            return Err(Error::Format(format!("paramètres BSI : en-tête {entete:?}")));
        }
        let mut parametres = Vec::new();
        for (i, l) in lignes.enumerate() {
            let n = i + 2;
            let c: Vec<&str> = l.split('\t').collect();
            if c.len() != attendu.len() {
                return Err(Error::Format(format!("paramètres BSI, ligne {n} : nombre de colonnes")));
            }
            let nombre = |s: &str, quoi: &str| -> Result<f64> {
                s.parse::<f64>()
                    .map_err(|_| Error::Format(format!("paramètres BSI, ligne {n} : {quoi}")))
            };
            let fonction = match c[9] {
                "flottant" => Fonction::Flottant,
                "octet" => Fonction::Octet,
                "long" => Fonction::Long,
                "decimal1" => Fonction::Decimal1,
                "chaine" => Fonction::Chaine,
                "brut" => Fonction::Brut,
                _ => Fonction::Inconnue,
            };
            let connue = fonction != Fonction::Inconnue;
            parametres.push(ParametreBsi {
                lid: champ_hex(c[0], "lid", n)?,
                rang: champ_hex(c[1], "rang", n)?,
                octet: champ_hex(c[2], "octet", n)?,
                bit: champ_hex(c[3], "bit", n)?,
                largeur: champ_hex(c[4], "largeur", n)?,
                nom: c[6].to_string(),
                genre: c[7].parse().map_err(|_| Error::Format(format!("paramètres BSI, ligne {n} : genre")))?,
                conversion: c[8]
                    .parse()
                    .map_err(|_| Error::Format(format!("paramètres BSI, ligne {n} : conversion")))?,
                fonction,
                petit_boutiste: c[10] == "le",
                facteur: if connue { nombre(c[11], "facteur")? } else { 1.0 },
                decalage: if connue { nombre(c[12], "decalage")? } else { 0.0 },
                masque: if c[13].is_empty() {
                    None
                } else {
                    Some(
                        u64::from_str_radix(c[13], 16)
                            .map_err(|_| Error::Format(format!("paramètres BSI, ligne {n} : masque")))?,
                    )
                },
                bits: if c[14].is_empty() {
                    0
                } else {
                    c[14].parse().map_err(|_| Error::Format(format!("paramètres BSI, ligne {n} : bits")))?
                },
            });
        }
        Ok(Self { parametres })
    }

    pub fn len(&self) -> usize {
        self.parametres.len()
    }

    pub fn is_empty(&self) -> bool {
        self.parametres.is_empty()
    }

    /// Paramètres connus de CAL264 pour une zone.
    pub fn parametres(&self, lid: u8) -> impl Iterator<Item = &ParametreBsi> {
        self.parametres.iter().filter(move |p| p.lid == lid)
    }

    /// Valeurs des paramètres d'une zone lue (`donnees` = octets après `61 LID`), dans l'ordre de CAL264 ; les
    /// paramètres que la BSI ne met pas dans cette zone sont omis, comme chez Lexia.
    pub fn valeurs(&self, lid: u8, donnees: &[u8], composition: &Composition, ini: &Ini) -> Result<Vec<(&ParametreBsi, Valeur)>> {
        let champs = composition.champs(lid, ini)?;
        let par_rang: BTreeMap<u16, &Champ> = champs.iter().rev().map(|c| (c.rang, c)).collect();
        Ok(self
            .parametres(lid)
            .filter_map(|p| {
                let c = par_rang.get(&u16::from(p.rang))?;
                Some((p, p.valeur(c, donnees)?))
            })
            .collect())
    }
}

/// Défaut lu dans les zones de défauts de la BSI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefautBsi {
    /// Code méthode (`2CZ`…) ; `ZZZ` si le rang n'a pas de code connu.
    pub code: String,
    /// `P` (présent) ou `F` (seulement dans la zone mémorisée).
    pub etat: char,
    pub lid: u8,
    pub rang: u16,
}

/// Codes de défaut de la BSI (`bsi_defauts_CAL264.tsv`) et tables de libellés par véhicule.
#[derive(Debug, Clone, Default)]
pub struct DefautsBsi {
    codes: BTreeMap<(u8, u16, char), String>,
    libelles: BTreeMap<String, String>,
}

impl DefautsBsi {
    pub fn charger(chemin: impl AsRef<Path>) -> Result<Self> {
        Self::lire(&std::fs::read_to_string(chemin)?)
    }

    pub fn lire(texte: &str) -> Result<Self> {
        let mut lignes = texte.lines();
        let attendu = ["nature", "vehicule", "lid", "rang", "etat", "code"];
        let entete: Vec<&str> = lignes.next().unwrap_or("").split('\t').collect();
        if entete != attendu {
            return Err(Error::Format(format!("défauts BSI : en-tête {entete:?}")));
        }
        let mut d = Self::default();
        for (i, l) in lignes.enumerate() {
            let c: Vec<&str> = l.split('\t').collect();
            let err = |quoi: &str| Error::Format(format!("défauts BSI, ligne {} : {quoi}", i + 2));
            if c.len() != attendu.len() {
                return Err(err("nombre de colonnes"));
            }
            match c[0] {
                "defaut" => {
                    let lid = u8::from_str_radix(c[2], 16).map_err(|_| err("lid"))?;
                    let rang = u16::from_str_radix(c[3], 16).map_err(|_| err("rang"))?;
                    let etat = c[4].chars().next().ok_or_else(|| err("etat"))?;
                    d.codes.insert((lid, rang, etat), c[5].to_string());
                }
                "libelles" => {
                    d.libelles.insert(c[1].to_string(), c[5].to_string());
                }
                _ => return Err(err("nature")),
            }
        }
        Ok(d)
    }

    pub fn len(&self) -> usize {
        self.codes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.codes.is_empty()
    }

    /// Table `SGML/Menu/<nom>.DBF` des libellés de la BSI pour ce véhicule.
    pub fn table_libelles(&self, vehicule: &str) -> Option<&str> {
        self.libelles.get(vehicule).map(String::as_str)
    }

    /// Défauts des zones lues (`trames` : zone -> données après `61 LID` ; une zone refusée par la BSI est
    /// absente), comme CAL264 `LECTDEF3020` : pour chaque paire (présents, mémorisés), si les deux zones existent,
    /// un bit de la première donne un défaut `P`, un bit de la seconde seule un défaut `F`, rangs pris dans la
    /// composition de la première ; si une seule existe (BSI de la C5 : zone 84 sans 83), ses bits donnent des
    /// défauts `P`. Triés par code, sans doublon.
    pub fn decoder(&self, trames: &BTreeMap<u8, Vec<u8>>, composition: &Composition, ini: &Ini) -> Result<Vec<DefautBsi>> {
        let mut out: Vec<DefautBsi> = Vec::new();
        for (pres, mem) in PAIRES_DEFAUTS {
            let dispo = |z: u8| composition.contient(z) && trames.contains_key(&z);
            let (lid, t1, t2): (u8, &[u8], &[u8]) = match (dispo(pres), dispo(mem)) {
                (true, true) => (pres, &trames[&pres], &trames[&mem]),
                (false, true) => (mem, &trames[&mem], &[]),
                (true, false) => (pres, &trames[&pres], &[]),
                (false, false) => continue,
            };
            let champs = composition.champs(lid, ini)?;
            for k in 0..t1.len().max(t2.len()) {
                for b in 0..8 {
                    let m = 1u8 << b;
                    let etat = if t1.get(k).is_some_and(|x| x & m != 0) {
                        'P'
                    } else if t2.get(k).is_some_and(|x| x & m != 0) {
                        'F'
                    } else {
                        continue;
                    };
                    let rel = k * 8 + b;
                    let rang = champs.get(rel).map_or(0xFFFF, |c| c.rang);
                    let code = self.codes.get(&(lid, rang, etat)).cloned().unwrap_or_else(|| "ZZZ".into());
                    let d = DefautBsi { code, etat, lid, rang };
                    if !out.iter().any(|x| x.code == d.code && x.etat == d.etat) {
                        out.push(d);
                    }
                }
            }
        }
        out.sort_by(|a, b| a.code.cmp(&b.code));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INI: &str = "[C9]\n0=b\n1=b\n2=B\n3=W\n4=b\n[84]\n0=b\n1=b\n2=b\n[87]\n0=b\n1=b\n";

    /// Réponse `75` d'un bloc de MAPPING.
    fn bloc(famille: u8, fin: bool, n: u32, donnees: &[u8]) -> Vec<u8> {
        let mut r = vec![0x75, famille, if fin { 0xff } else { 0x80 }];
        r.extend_from_slice(&n.to_be_bytes()[1..]);
        r.push(donnees.len() as u8);
        r.extend_from_slice(donnees);
        r.extend_from_slice(&[0xab, 0xcd]);
        r
    }

    #[test]
    fn composition_et_decoupage() {
        let ini = Ini::lire(INI);
        // famille c0 en deux blocs (dans le désordre) : zone c9 = rangs 3, 0, 1, 2 puis un bit de bourrage, 4
        let r = [bloc(0xc0, true, 2, &[0xfe, 0x04]), bloc(0xc0, false, 1, &[0x09, 0x03, 0x00, 0x01, 0x02])];
        let c = Composition::depuis_mapping(r.iter().map(Vec::as_slice));
        assert_eq!(c.zones().collect::<Vec<_>>(), [0xc9]);
        let ch = c.champs(0xc9, &ini).unwrap();
        let resume: Vec<(u16, usize, u8)> = ch.iter().map(|c| (c.rang, c.position, c.masque)).collect();
        // W en 0-1, bits 0 et 1 dans l'octet 2, B en 3, bourrage bit puis bit 4 dans l'octet 4
        assert_eq!(resume, [(3, 0, 0), (0, 2, 1), (1, 2, 2), (2, 3, 0), (0x100, 4, 1), (4, 4, 2)]);
        assert_eq!(c.taille(0xc9, &ini).unwrap(), 5);
        assert!(c.champs(0xc0, &ini).is_err());
        // rang inconnu de BSI.ini : erreur ; rang répété : bourrage
        let r = [bloc(0xc0, true, 1, &[0x09, 0x07])];
        assert!(Composition::depuis_mapping(r.iter().map(Vec::as_slice)).champs(0xc9, &ini).is_err());
        let r = [bloc(0xc0, true, 1, &[0x09, 0x02, 0x02])];
        let ch = Composition::depuis_mapping(r.iter().map(Vec::as_slice)).champs(0xc9, &ini).unwrap();
        assert_eq!((ch[1].rang, ch[1].position), (0x100, 1));
    }

    #[test]
    fn neuf_bits_a_la_suite() {
        let ini = Ini::lire("[84]\n0=b\n1=b\n2=b\n3=b\n4=b\n5=b\n6=b\n7=b\n8=b\n");
        let r = [bloc(0x83, true, 1, &[0x01, 0, 1, 2, 3, 4, 5, 6, 7, 8])];
        let c = Composition::depuis_mapping(r.iter().map(Vec::as_slice));
        let ch = c.champs(0x84, &ini).unwrap();
        assert_eq!((ch[7].position, ch[7].masque), (0, 0x80));
        assert_eq!((ch[8].position, ch[8].masque), (1, 0x01));
    }

    fn param(fonction: Fonction, facteur: f64, decalage: f64) -> ParametreBsi {
        ParametreBsi {
            lid: 0xc9,
            rang: 0,
            octet: 0xff,
            bit: 0xff,
            largeur: 0xff,
            nom: "X".into(),
            genre: 4,
            conversion: 0,
            fonction,
            petit_boutiste: false,
            facteur,
            decalage,
            masque: None,
            bits: 0,
        }
    }

    #[test]
    fn conversions() {
        let octet = Champ { rang: 0, type_param: TypeParam::Octet, position: 0, masque: 0 };
        let mot = Champ { rang: 0, type_param: TypeParam::Mot, position: 0, masque: 0 };
        // température d'eau (conversion 9) : x - 40 ; 0xff invalide
        let eau = param(Fonction::Flottant, 1.0, -40.0);
        assert_eq!(eau.valeur(&octet, &[0x7b]), Some(Valeur::Nombre(83.0)));
        assert_eq!(eau.valeur(&octet, &[0xff]), Some(Valeur::Invalide));
        // vitesse (conversion 10) : petit-boutiste × 0,01
        let mut vitesse = param(Fonction::Flottant, 0.01, 0.0);
        vitesse.petit_boutiste = true;
        assert_eq!(vitesse.valeur(&mot, &[0x34, 0x12]), Some(Valeur::Nombre(0x1234 as f64 * 0.01)));
        // quartet haut (conversion 36)
        let mut q = param(Fonction::Flottant, 1.0, 0.0);
        q.masque = Some(0xf);
        q.bits = 4;
        assert_eq!(q.valeur(&octet, &[0xa5]), Some(Valeur::Nombre(10.0)));
        // entier long : tout à 1 invalide
        let l = param(Fonction::Long, 1.0, 0.0);
        assert_eq!(l.valeur(&mot, &[0x12, 0x34]), Some(Valeur::Nombre(4660.0)));
        assert_eq!(l.valeur(&mot, &[0xff, 0xff]), Some(Valeur::Invalide));
        // bit : 0 ou 1, quel que soit son masque
        let b = Champ { rang: 0, type_param: TypeParam::Bit, position: 1, masque: 0x10 };
        assert_eq!(param(Fonction::Octet, 1.0, 0.0).valeur(&b, &[0, 0x10]), Some(Valeur::Nombre(1.0)));
        assert_eq!(param(Fonction::Octet, 1.0, 0.0).valeur(&b, &[0xff, 0xef]), Some(Valeur::Nombre(0.0)));
        // sous-champ : bits 4 à 7 gardés en place
        let mut s = param(Fonction::Octet, 1.0, 0.0);
        (s.octet, s.bit, s.largeur) = (0, 4, 4);
        assert_eq!(s.valeur(&octet, &[0x3c]), Some(Valeur::Nombre(48.0)));
        // décimal à un chiffre
        let d = param(Fonction::Decimal1, 0.1, 0.0);
        let dm = Champ { rang: 0, type_param: TypeParam::DoubleMot, position: 0, masque: 0 };
        assert_eq!(d.valeur(&dm, &[0, 1, 0xe2, 0x40]), Some(Valeur::Nombre(12345.6)));
        assert_eq!(d.valeur(&dm, &[0, 1]), None);
    }

    #[test]
    fn dictionnaires() {
        let p = "lid\trang\toctet\tbit\tlargeur\ttaille\tnom\tgenre\tconversion\tfonction\tordre\tfacteur\tdecalage\tmasque\tbits\n\
                 c9\t02\tff\tff\tff\t1\tTEMPEAU\t4\t9\tflottant\tbe\t1.0\t-40.0\t\t0\n";
        let d = DictionnaireBsi::lire(p).unwrap();
        assert_eq!(d.len(), 1);
        let ini = Ini::lire(INI);
        let r = [bloc(0xc0, true, 1, &[0x09, 0x02])];
        let c = Composition::depuis_mapping(r.iter().map(Vec::as_slice));
        let v = d.valeurs(0xc9, &[0x7b], &c, &ini).unwrap();
        assert_eq!((v[0].0.nom.as_str(), &v[0].1), ("TEMPEAU", &Valeur::Nombre(83.0)));
        assert!(DictionnaireBsi::lire("x\n").is_err());

        let t = "nature\tvehicule\tlid\trang\tetat\tcode\n\
                 defaut\t*\t84\t01\tP\t6PZ\ndefaut\t*\t87\t00\tP\t63Z\nlibelles\tCITROENC5\t\t\t\tMENU906N\n";
        let def = DefautsBsi::lire(t).unwrap();
        assert_eq!(def.table_libelles("CITROENC5"), Some("MENU906N"));
        // BSI sans zone 83 : la zone 84 seule donne des défauts « P »
        let r = [bloc(0x83, true, 1, &[0x01, 0x02, 0x01, 0xff, 0x04, 0x00, 0x01])];
        let c = Composition::depuis_mapping(r.iter().map(Vec::as_slice));
        let mut trames = BTreeMap::new();
        trames.insert(0x84, vec![0x02]);
        trames.insert(0x87, vec![0x00]);
        let d = def.decoder(&trames, &c, &ini).unwrap();
        assert_eq!(d.iter().map(|d| (d.code.as_str(), d.etat, d.rang)).collect::<Vec<_>>(), [("6PZ", 'P', 1)]);
        trames.insert(0x84, vec![0x00]);
        assert!(def.decoder(&trames, &c, &ini).unwrap().is_empty());
        trames.insert(0x87, vec![0x01]);
        let d = def.decoder(&trames, &c, &ini).unwrap();
        assert_eq!(d[0].code, "63Z");
        // rang sans code : ZZZ
        trames.insert(0x84, vec![0x01]);
        assert!(def.decoder(&trames, &c, &ini).unwrap().iter().any(|d| d.code == "ZZZ"));
    }

    #[test]
    fn liste_ini() {
        let t = "[BSM]\nFamillesLID = C0,B0,83\n[BSI]\nNom =bsi.ini\nFamillesLID = C0,B0,D0,A0,83\n";
        assert_eq!(familles_liste_ini(t, "BSI"), Some(FAMILLES.to_vec()));
        assert_eq!(familles_liste_ini(t, "XXX"), None);
    }
}
