//! Lecteur (lecture seule) des bases Firebird / InterBase ODS 10, sans moteur Firebird.
//!
//! ```text
//! pag  : u8 type, u8 flags, u16 checksum, u32 generation, u32 seqno, u32 offset        (16 o)
//! dpg  : pag + i32 sequence, u16 relation, u16 count, puis count × (u16 offset, u16 length)
//! rhd  : i32 transaction, i32 b_page, u16 b_line, u16 flags, u8 format, données (13 o)
//! rhdf : rhd + i32 f_page @16, u16 f_line @20, données @22
//! flags rhd : 1 supprimé, 2 ancienne version, 4 fragment, 8 incomplet, 16 blob
//! enregistrement : RLE (n > 0 : n octets littéraux ; n < 0 : octet suivant répété -n fois)
//! ```

use crate::text::{decode, Codepage};
use crate::{rd, Error, Result};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

const PAGE_DATA: u8 = 5;
const REL_FIELDS: u16 = 5; // RDB$RELATION_FIELDS
const REL_RELATIONS: u16 = 6; // RDB$RELATIONS
const REL_FORMATS: u16 = 8; // RDB$FORMATS

const F_DELETED: u16 = 1;
const F_CHAIN: u16 = 2;
const F_FRAGMENT: u16 = 4;
const F_INCOMPLETE: u16 = 8;
const F_BLOB: u16 = 16;

/// Niveau page : enregistrements bruts et blobs.
pub struct Ods10 {
    d: Vec<u8>,
    pub page_size: usize,
    pub ods: u16,
    pub npages: usize,
    /// relation → { séquence → page de données }
    dp: HashMap<u16, BTreeMap<i32, usize>>,
    max_records: i64,
}

/// Enregistrement décompressé.
pub struct Record {
    pub number: i64,
    pub format: u8,
    pub data: Vec<u8>,
}

impl Ods10 {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_bytes(std::fs::read(path)?)
    }

    pub fn from_bytes(d: Vec<u8>) -> Result<Self> {
        let page_size = rd::u16(&d, 16, "en-tête")? as usize;
        let ods = rd::u16(&d, 18, "en-tête")?;
        if ods & 0x7FFF != 10 {
            return Err(Error::Format(format!("ODS {ods} non géré")));
        }
        if page_size < 1024 {
            return Err(Error::Format(format!("taille de page {page_size}")));
        }
        let npages = d.len() / page_size;
        let mut dp: HashMap<u16, BTreeMap<i32, usize>> = HashMap::new();
        for p in 0..npages {
            let o = p * page_size;
            if d[o] == PAGE_DATA {
                let seq = rd::i32(&d, o + 16, "dpg")?;
                let rel = rd::u16(&d, o + 20, "dpg")?;
                dp.entry(rel).or_default().insert(seq, p);
            }
        }
        let max_records = ((page_size - 28) / (4 + 13)) as i64;
        Ok(Self {
            d,
            page_size,
            ods,
            npages,
            dp,
            max_records,
        })
    }

    fn page(&self, p: usize) -> Result<&[u8]> {
        self.d
            .get(p * self.page_size..(p + 1) * self.page_size)
            .ok_or(Error::Truncated {
                what: "page",
                offset: p,
            })
    }

    /// Octets de l'emplacement `line` de la page de données `p`.
    fn slot(&self, p: usize, line: usize) -> Result<&[u8]> {
        let pg = self.page(p)?;
        let off = rd::u16(pg, 24 + 4 * line, "emplacement")? as usize;
        let ln = rd::u16(pg, 26 + 4 * line, "emplacement")? as usize;
        Ok(rd::slice(pg, off, ln))
    }

    fn rle(b: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(b.len() * 2);
        let mut i = 0;
        while i < b.len() {
            let n = b[i] as i8;
            i += 1;
            if n > 0 {
                out.extend_from_slice(rd::slice(b, i, n as usize));
                i += n as usize;
            } else if n < 0 {
                let Some(&c) = b.get(i) else { break };
                out.resize(out.len() + n.unsigned_abs() as usize, c);
                i += 1;
            }
        }
        out
    }

    /// Données compressées complètes d'un enregistrement, fragments suivis.
    fn record_raw(&self, rec: &[u8]) -> Result<Vec<u8>> {
        let flags = rd::u16(rec, 10, "rhd")?;
        if flags & F_INCOMPLETE == 0 {
            return Ok(rd::slice(rec, 13, usize::MAX).to_vec());
        }
        let mut data = rd::slice(rec, 22, usize::MAX).to_vec();
        let mut next = (rd::i32(rec, 16, "rhdf")?, rd::u16(rec, 20, "rhdf")?);
        for _ in 0..self.npages.max(1) * 256 {
            let page = usize::try_from(next.0).map_err(|_| Error::Format("page de fragment négative".into()))?;
            let frag = self.slot(page, next.1 as usize)?;
            if rd::u16(frag, 10, "fragment")? & F_INCOMPLETE != 0 {
                next = (rd::i32(frag, 16, "fragment")?, rd::u16(frag, 20, "fragment")?);
                data.extend_from_slice(rd::slice(frag, 22, usize::MAX));
            } else {
                data.extend_from_slice(rd::slice(frag, 13, usize::MAX));
                return Ok(data);
            }
        }
        Err(Error::Format("chaîne de fragments sans fin".into()))
    }

    /// Enregistrements primaires (version courante, non supprimés) d'une relation.
    pub fn records(&self, rel: u16) -> Result<Vec<Record>> {
        let mut out = Vec::new();
        let Some(pages) = self.dp.get(&rel) else {
            return Ok(out);
        };
        for (&seq, &p) in pages {
            let count = rd::u16(self.page(p)?, 22, "dpg")? as usize;
            for line in 0..count {
                let rec = self.slot(p, line)?;
                if rec.is_empty() {
                    continue;
                }
                let flags = rd::u16(rec, 10, "rhd")?;
                if flags & (F_DELETED | F_CHAIN | F_FRAGMENT | F_BLOB) != 0 {
                    continue;
                }
                out.push(Record {
                    number: i64::from(seq) * self.max_records + line as i64,
                    format: rd::u8(rec, 12, "rhd")?,
                    data: Self::rle(&self.record_raw(rec)?),
                });
            }
        }
        Ok(out)
    }

    /// Blob de niveau 0 ou 1 ; `None` si l'identifiant ne désigne pas un blob lisible.
    pub fn blob(&self, rel: u16, number: u32) -> Result<Option<Vec<u8>>> {
        let (seq, line) = (
            i64::from(number) / self.max_records,
            i64::from(number) % self.max_records,
        );
        let Some(&p) = self.dp.get(&rel).and_then(|m| m.get(&(seq as i32))) else {
            return Ok(None);
        };
        let rec = self.slot(p, line as usize)?;
        // enregistrement blob = blh sans rhd : +10 flags, +12 niveau, +16 nombre de segments, +20 longueur, +28 données
        if rec.len() < 28 || rd::u16(rec, 10, "blh")? & F_BLOB == 0 {
            return Ok(None);
        }
        let bflags = rd::u16(rec, 10, "blh")?;
        let level = rec[12];
        let count = rd::u32(rec, 16, "blh")? as usize;
        let length = rd::u32(rec, 20, "blh")? as usize;
        let body = &rec[28..];
        let total = length.saturating_add(2 * count);
        let raw: Vec<u8> = match level {
            0 => rd::slice(body, 0, total).to_vec(),
            1 => {
                // vecteur de numéros de page ; page de blob : pag(16) + lead(4) + séquence(4) + longueur(2) + pad(2) + données à +28
                let mut raw = Vec::new();
                for k in 0..body.len() / 4 {
                    let bp = rd::i32(body, 4 * k, "blob pages")?;
                    if bp <= 0 {
                        break;
                    }
                    let bpg = self.page(bp as usize)?;
                    let blen = rd::u16(bpg, 24, "page de blob")? as usize;
                    raw.extend_from_slice(rd::slice(bpg, 28, blen));
                }
                raw.truncate(total);
                raw
            }
            _ => return Ok(None),
        };
        if bflags & 32 != 0 {
            // blob en mode flux
            return Ok(Some(rd::slice(&raw, 0, length).to_vec()));
        }
        let mut out = Vec::with_capacity(length);
        let mut i = 0;
        while i + 2 <= raw.len() && out.len() < length {
            let sl = rd::u16(&raw, i, "segment")? as usize;
            out.extend_from_slice(rd::slice(&raw, i + 2, sl));
            i += 2 + sl;
        }
        Ok(Some(out))
    }
}

/// Descripteur de champ (`dsc` de 12 octets de `RDB$FORMATS.RDB$DESCRIPTOR`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dsc {
    pub dtype: u8,
    pub scale: i8,
    pub length: u16,
    pub sub_type: i16,
    pub offset: u32,
}

pub fn dtype_name(dtype: u8) -> Option<&'static str> {
    Some(match dtype {
        1 => "CHAR",
        2 => "CSTRING",
        3 => "VARCHAR",
        7 => "BYTE",
        8 => "SMALLINT",
        9 => "INTEGER",
        10 => "QUAD",
        11 => "FLOAT",
        12 => "DOUBLE",
        14 => "DATE",
        15 => "TIME",
        16 => "TIMESTAMP",
        17 => "BLOB",
        18 => "ARRAY",
        19 => "BIGINT",
        _ => return None,
    })
}

fn parse_format(blob: &[u8]) -> Vec<Dsc> {
    blob.as_chunks::<12>()
        .0
        .iter()
        .map(|c| Dsc {
            dtype: c[0],
            scale: c[1] as i8,
            length: u16::from_le_bytes([c[2], c[3]]),
            sub_type: i16::from_le_bytes([c[4], c[5]]),
            offset: u32::from_le_bytes([c[8], c[9], c[10], c[11]]),
        })
        .collect()
}

/// Valeur d'un champ.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Int(i64),
    /// Flottant, ou entier avec échelle décimale (`NUMERIC`).
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
}

impl Value {
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(v) => Some(*v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Float(v) => Some(*v),
            Value::Int(v) => Some(*v as f64),
            _ => None,
        }
    }
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }
}

/// Date civile à partir d'un nombre de jours depuis le 17/11/1858 (jour julien modifié).
fn date_mjd(days: i32) -> String {
    // algorithme « civil_from_days » (Howard Hinnant), époque 1970-01-01 = MJD 40587
    let z = i64::from(days) - 40587 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Une ligne : (nom de colonne, valeur), dans l'ordre des champs.
pub type Row = Vec<(String, Value)>;

/// Accès par noms : tables utilisateur, colonnes, lignes décodées.
pub struct Database {
    pub ods: Ods10,
    pub codepage: Codepage,
    fmts: HashMap<(i16, i16), Vec<Dsc>>,
    rel_by_name: BTreeMap<String, i64>,
    cols: HashMap<String, BTreeMap<i64, String>>,
}

impl Database {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::new(Ods10::open(path)?, Codepage::Cp1252)
    }

    pub fn new(ods: Ods10, codepage: Codepage) -> Result<Self> {
        let mut db = Self {
            ods,
            codepage,
            fmts: HashMap::new(),
            rel_by_name: BTreeMap::new(),
            cols: HashMap::new(),
        };
        for r in db.ods.records(REL_FORMATS)? {
            let relid = rd::i16(&r.data, 4, "RDB$FORMATS")?;
            let fno = rd::i16(&r.data, 6, "RDB$FORMATS")?;
            let brel = rd::u32(&r.data, 8, "RDB$FORMATS")?;
            let bnum = rd::u32(&r.data, 12, "RDB$FORMATS")?;
            let blob = db.ods.blob(brel as u16, bnum)?.unwrap_or_default();
            db.fmts.insert((relid, fno), parse_format(&blob));
        }
        for r in db.ods.records(REL_RELATIONS)? {
            let f = db.fmt(REL_RELATIONS as i16, i16::from(r.format))?;
            let (rid, name) = (db.field(&r.data, f, 3, false)?, db.field(&r.data, f, 8, false)?);
            if let (Some(rid), Value::Text(name)) = (rid.as_i64(), name) {
                db.rel_by_name.insert(name, rid);
            }
        }
        for r in db.ods.records(REL_FIELDS)? {
            let f = db.fmt(REL_FIELDS as i16, i16::from(r.format))?;
            let (name, rel, id) = (
                db.field(&r.data, f, 0, false)?,
                db.field(&r.data, f, 1, false)?,
                db.field(&r.data, f, 9, false)?,
            );
            if let (Value::Text(name), Value::Text(rel), Some(id)) = (name, rel, id.as_i64()) {
                db.cols.entry(rel).or_default().insert(id, name);
            }
        }
        Ok(db)
    }

    /// Format `fno` de la relation, ou à défaut son premier format stocké (format d'amorçage 0).
    fn fmt(&self, rel: i16, fno: i16) -> Result<&[Dsc]> {
        if let Some(f) = self.fmts.get(&(rel, fno)) {
            return Ok(f);
        }
        self.fmts
            .iter()
            .filter(|(k, _)| k.0 == rel)
            .min_by_key(|(k, _)| k.1)
            .map(|(_, f)| f.as_slice())
            .ok_or_else(|| Error::Format(format!("aucun format pour la relation {rel}")))
    }

    fn field(&self, rec: &[u8], f: &[Dsc], idx: usize, blobs: bool) -> Result<Value> {
        let d = f
            .get(idx)
            .ok_or_else(|| Error::Format(format!("champ {idx} absent du format")))?;
        self.value(rec, d, idx, blobs)
    }

    /// Valeur du champ `idx` (bit « nul » `idx` en tête d'enregistrement) selon son descripteur.
    fn value(&self, rec: &[u8], d: &Dsc, idx: usize, blobs: bool) -> Result<Value> {
        if rd::u8(rec, idx >> 3, "bits nuls")? & (1 << (idx & 7)) != 0 {
            return Ok(Value::Null);
        }
        let o = d.offset as usize;
        let b = rd::slice(rec, o, d.length as usize);
        let v: i64 = match d.dtype {
            1 => {
                return Ok(Value::Text(
                    decode(b, self.codepage).trim_end_matches(is_py_space).to_string(),
                ))
            }
            3 => {
                let n = rd::u16(b, 0, "VARCHAR")? as usize;
                return Ok(Value::Text(decode(rd::slice(rec, o + 2, n), self.codepage)));
            }
            8 => i64::from(rd::i16(b, 0, "SMALLINT")?),
            9 | 14 | 15 => i64::from(rd::i32(b, 0, "INTEGER")?),
            19 => rd::i64(b, 0, "BIGINT")?,
            11 => return Ok(Value::Float(f64::from(f32::from_bits(rd::u32(b, 0, "FLOAT")?)))),
            12 => return Ok(Value::Float(f64::from_bits(rd::i64(b, 0, "DOUBLE")? as u64))),
            16 => {
                let (days, frac) = (rd::i32(b, 0, "TIMESTAMP")?, rd::u32(b, 4, "TIMESTAMP")?);
                return Ok(Value::Text(format!(
                    "{} {:02}:{:02}",
                    date_mjd(days),
                    frac / 36_000_000,
                    frac / 600_000 % 60
                )));
            }
            17 => {
                let (brel, bnum) = (rd::u32(b, 0, "BLOB")?, rd::u32(b, 4, "BLOB")?);
                if !blobs || (brel == 0 && bnum == 0) {
                    return Ok(Value::Null);
                }
                return Ok(match self.ods.blob(brel as u16, bnum)? {
                    None => Value::Text(format!("<blob {brel}:{bnum}>")),
                    Some(raw) if d.sub_type == 1 => Value::Text(decode(&raw, self.codepage)),
                    Some(raw) => Value::Bytes(raw),
                });
            }
            _ => return Ok(Value::Text(b.iter().map(|c| format!("{c:02x}")).collect())),
        };
        Ok(if d.scale < 0 {
            Value::Float(v as f64 / 10f64.powi(-i32::from(d.scale)))
        } else {
            Value::Int(v)
        })
    }

    /// Tables utilisateur (identifiant de relation ≥ 128), triées.
    pub fn tables(&self) -> Vec<&str> {
        self.rel_by_name
            .iter()
            .filter(|(_, &i)| i >= 128)
            .map(|(n, _)| n.as_str())
            .collect()
    }

    /// Colonnes d'une table, dans l'ordre des identifiants de champ.
    pub fn columns(&self, table: &str) -> Vec<&str> {
        self.cols
            .get(table)
            .map(|c| c.values().map(String::as_str).collect())
            .unwrap_or_default()
    }

    fn rel_id(&self, table: &str) -> Result<i16> {
        self.rel_by_name
            .get(table)
            .and_then(|&i| i16::try_from(i).ok())
            .ok_or_else(|| Error::Format(format!("table inconnue : {table}")))
    }

    /// Schéma (format le plus récent) : (colonne, descripteur).
    pub fn schema(&self, table: &str) -> Result<Vec<(&str, Dsc)>> {
        let rid = self.rel_id(table)?;
        let f = self
            .fmts
            .iter()
            .filter(|(k, _)| k.0 == rid)
            .max_by_key(|(k, _)| k.1)
            .map(|(_, f)| f)
            .ok_or_else(|| Error::Format(format!("aucun format pour {table}")))?;
        Ok(self.columns(table).into_iter().zip(f.iter().copied()).collect())
    }

    /// Lignes d'une table. `blobs` : lire aussi le contenu des blobs (sinon `Null`).
    pub fn rows(&self, table: &str, blobs: bool) -> Result<Vec<Row>> {
        let rid = self.rel_id(table)?;
        let names = self.columns(table);
        let mut out = Vec::new();
        for r in self.ods.records(rid as u16)? {
            let f = self.fmt(rid, i16::from(r.format))?;
            let mut row = Row::with_capacity(f.len());
            for (i, d) in f.iter().enumerate() {
                let name = names.get(i).map_or_else(|| format!("F{i}"), |n| n.to_string());
                row.push((name, self.value(&r.data, d, i, blobs)?));
            }
            out.push(row);
        }
        Ok(out)
    }

    /// Lignes d'une table dont la colonne entière `colonne` vaut une des `cles`. Seule cette colonne est
    /// décodée pour les autres lignes : c'est le moyen de lire quelques lignes, blobs compris, d'une grande table.
    pub fn rows_cle(&self, table: &str, blobs: bool, colonne: &str, cles: &HashSet<i64>) -> Result<Vec<Row>> {
        let rid = self.rel_id(table)?;
        let names = self.columns(table);
        let Some(k) = names.iter().position(|n| *n == colonne) else {
            return Err(Error::Format(format!("colonne inconnue : {table}.{colonne}")));
        };
        let mut out = Vec::new();
        for r in self.ods.records(rid as u16)? {
            let f = self.fmt(rid, i16::from(r.format))?;
            let Some(d) = f.get(k) else { continue };
            if !self
                .value(&r.data, d, k, false)?
                .as_i64()
                .is_some_and(|v| cles.contains(&v))
            {
                continue;
            }
            let mut row = Row::with_capacity(f.len());
            for (i, d) in f.iter().enumerate() {
                let name = names.get(i).map_or_else(|| format!("F{i}"), |n| n.to_string());
                row.push((name, self.value(&r.data, d, i, blobs)?));
            }
            out.push(row);
        }
        Ok(out)
    }
}

/// Valeur de la colonne `name` d'une ligne.
pub fn col<'a>(row: &'a Row, name: &str) -> Option<&'a Value> {
    row.iter().find(|(n, _)| n == name).map(|(_, v)| v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rle() {
        assert_eq!(Ods10::rle(&[3, b'a', b'b', b'c', 0xFD, b'z', 1, b'!']), b"abczzz!");
        assert_eq!(Ods10::rle(&[5, b'a']), b"a");
        assert_eq!(Ods10::rle(&[0xFE]), b"");
    }

    #[test]
    fn dates() {
        assert_eq!(date_mjd(0), "1858-11-17");
        assert_eq!(date_mjd(40587), "1970-01-01");
        assert_eq!(date_mjd(51603), "2000-02-29");
        assert_eq!(date_mjd(56208), "2012-10-08");
    }

    #[test]
    fn refuse_autre_ods() {
        let mut d = vec![0u8; 4096];
        d[16..18].copy_from_slice(&4096u16.to_le_bytes());
        d[18..20].copy_from_slice(&11u16.to_le_bytes());
        assert!(Ods10::from_bytes(d).is_err());
        assert!(Ods10::from_bytes(vec![0; 8]).is_err());
    }
}
