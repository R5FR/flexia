//! Empreintes des données lues par `psa-data`, à comparer avec `tools/crosscheck.py` (parseurs Python).
//! Usage : digest --lexia <APP/LEXIA> --common <APP/COMMON> --fdb <dossier des copies .FDB>

use psa_data::descr::{self, ProRec};
use psa_data::dico::{Dico, Kind};
use psa_data::ods10::{Database, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const BASES: [&str; 4] = ["DSD", "GENERATIONDICOV2", "GPC", "QUICKDEF"];

fn hex(b: &[u8]) -> String {
    b.iter().map(|c| format!("{c:02x}")).collect()
}

fn fichiers(dir: &Path, exts: &[&str], out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            fichiers(&p, exts, out);
        } else if p
            .extension()
            .is_some_and(|x| exts.contains(&x.to_string_lossy().to_ascii_lowercase().as_str()))
        {
            out.push(p);
        }
    }
}

fn rel(p: &Path, base: &Path) -> String {
    p.strip_prefix(base)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().collect();
    let arg = |k: &str| {
        a.iter()
            .position(|x| x == k)
            .and_then(|i| a.get(i + 1))
            .map(PathBuf::from)
    };
    let mut lignes = Vec::new();

    if let Some(dir) = arg("--fdb") {
        for base in BASES {
            let db = Database::open(dir.join(format!("{base}.FDB")))?;
            for t in db.tables() {
                let rows = db.rows(t, true)?;
                let mut h = Sha256::new();
                for row in &rows {
                    for (k, v) in row {
                        h.update(k.as_bytes());
                        h.update(b"=");
                        match v {
                            Value::Null => h.update(b"\\N"),
                            Value::Int(i) => h.update(i.to_string().as_bytes()),
                            Value::Float(f) => h.update(hex(&f.to_le_bytes()).as_bytes()),
                            Value::Text(s) => {
                                h.update(b"s:");
                                h.update(s.as_bytes())
                            }
                            Value::Bytes(b) => {
                                h.update(b"b:");
                                h.update(hex(b).as_bytes())
                            }
                        }
                        h.update(b"\x1f");
                    }
                    h.update(b"\x1e");
                }
                lignes.push(format!(
                    "fdb\t{base}/{t}\t{}\t{}",
                    rows.len(),
                    hex(&h.finalize())
                ));
            }
        }
    }

    if let Some(lx) = arg("--lexia") {
        let dir = lx.join("DICO");
        let mut fs = Vec::new();
        fichiers(&dir, &["dbz", "zcl"], &mut fs);
        for p in fs {
            let d = Dico::open(&p)?;
            let (mut hr, mut ht) = (Sha256::new(), Sha256::new());
            for i in 0..d.len() {
                hr.update(d.raw(i).ok_or("chaîne hors fichier")?);
                hr.update(b"\0");
                ht.update(d.get(i).unwrap().as_bytes());
                ht.update(b"\n");
            }
            let c = d.check();
            lignes.push(format!(
                "dico\t{}\t{}:{}:{}:{}{}{}{}:{}\t{}\t{}",
                rel(&p, &dir),
                if d.kind == Kind::Dbz { "DBZ" } else { "ZCL" },
                d.name.as_deref().unwrap_or("-"),
                d.len(),
                c.monotone as u8,
                c.first_at_data_offset as u8,
                c.in_bounds as u8,
                c.last_string_ends_at_eof as u8,
                c.empty,
                hex(&hr.finalize()),
                hex(&ht.finalize())
            ));
        }
    }

    if let Some(com) = arg("--common") {
        let mut fs = Vec::new();
        fichiers(&com, &["pc", "68k", "pro"], &mut fs);
        for p in fs {
            let data = std::fs::read(&p)?;
            let ext = p
                .extension()
                .unwrap()
                .to_string_lossy()
                .to_ascii_lowercase();
            let mut s = String::new();
            let n;
            match ext.as_str() {
                "pc" => {
                    let rows = descr::parse_pc(&data);
                    n = rows.len();
                    for r in rows {
                        s += &format!("{}|{}|{}|{}\n", r.classe, r.code, r.delai, r.libelle);
                    }
                }
                "68k" => {
                    let recs = descr::parse_68k(&data);
                    n = recs.len();
                    for r in &recs {
                        let l = r.ligne.map_or(String::new(), |l| {
                            format!("{},{},{},{},{}", l.start, l.data, l.stop, l.parite, l.bauds)
                        });
                        s += &format!("{}|{}|{}\n", r.raw, r.id, l);
                    }
                    let (cmds, divers) = descr::commandes_68k(&recs);
                    for (com, c) in cmds {
                        s += &format!(
                            "{com}:H={};T={};S={};D={};R={}\n",
                            c.h.join(","),
                            c.t.join(","),
                            c.s.join(","),
                            c.d.join(","),
                            c.r as u8
                        );
                    }
                    for d in divers {
                        s += &format!("X:{d}\n");
                    }
                }
                _ => {
                    let recs = descr::parse_pro(&data)?;
                    n = recs.len();
                    for r in recs {
                        s += &match r {
                            ProRec::Bloc { tag, a, b } => format!("{tag},{a},{b}\n"),
                            ProRec::Court { tag, valeur } => format!("{tag},{valeur},-\n"),
                            ProRec::Val { tag, index, valeur } => {
                                format!("{tag},{index},{valeur}\n")
                            }
                            ProRec::Inconnu { offset, octets } => {
                                format!("?,{offset},{}\n", hex(&octets))
                            }
                        };
                    }
                }
            }
            lignes.push(format!(
                "{ext}\t{}\t{n}\t{}",
                rel(&p, &com),
                hex(&Sha256::digest(s.as_bytes()))
            ));
        }
    }

    lignes.sort();
    for l in lignes {
        println!("{l}");
    }
    Ok(())
}
