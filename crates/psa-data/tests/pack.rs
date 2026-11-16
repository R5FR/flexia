//! Tests sur les fichiers réels du pack (mêmes contrôles que `parsers/test_parsers.py`).
//! Ignorés si les variables d'environnement ne sont pas définies :
//! `FLEXIA_LEXIA` (…/APP/LEXIA), `FLEXIA_COMMON` (…/APP/COMMON), `FLEXIA_FDB` (copies des `.FDB`).

use psa_data::descr::{self, ProRec};
use psa_data::dico::{Dico, Kind};
use psa_data::ods10::{col, Database};
use std::path::PathBuf;

fn dossier(var: &str) -> Option<PathBuf> {
    let d = std::env::var_os(var).map(PathBuf::from);
    if d.is_none() {
        eprintln!("ignoré : {var} non défini");
    }
    d
}

#[test]
fn dictionnaires() {
    let Some(lx) = dossier("FLEXIA_LEXIA") else {
        return;
    };
    for (f, n, k) in [
        ("DICO/CITTTFR.DBZ", 82, Kind::Dbz),
        ("DICO/citroen/dicoSFR.zcl", 5801, Kind::Zcl),
        ("DICO/citroen/dicodRU.zcl", 14846, Kind::Zcl),
    ] {
        let d = Dico::open(lx.join(f)).unwrap();
        let c = d.check();
        assert!(
            d.kind == k && d.len() == n && c.monotone && c.last_string_ends_at_eof,
            "{f}"
        );
    }
    let d = Dico::open(lx.join("DICO/citroen/dicodFR.zcl")).unwrap();
    assert_eq!(d.get(3030).as_deref(), Some("consommation instantanée"));
}

#[test]
fn descripteurs_vci() {
    let Some(com) = dossier("FLEXIA_COMMON") else {
        return;
    };
    let lire = |f: &str| std::fs::read(com.join(f)).unwrap();
    let rows = descr::parse_pc(&lire("SUPER.PC"));
    assert_eq!(rows.len(), 39);
    assert_eq!((rows[1].libelle.as_str(), rows[1].delai), ("BOOT - ECHO", 4000));
    assert!(descr::parse_68k(&lire("P_5B.68K"))
        .iter()
        .any(|r| r.ligne.is_some_and(|l| l.bauds == 10400)));
    for (f, tag, index, valeur) in [
        ("P_UDSMCV.pro", 'Q', 1, 0x18DA_06A0),
        ("P_CAN.pro", 'P', 1, 0x6A7),
        ("P_KWPOnCAN.pro", 'f', 5, 0xF1),
    ] {
        let recs = descr::parse_pro(&lire(f)).unwrap();
        assert!(!recs.iter().any(|r| matches!(r, ProRec::Inconnu { .. })), "{f}");
        // comme le dictionnaire Python : la dernière valeur de (lettre, index) l'emporte
        let v = recs.iter().rev().find_map(|r| match r {
            ProRec::Val {
                tag: t,
                index: i,
                valeur,
            } if (*t, *i) == (tag, index) => Some(*valeur),
            _ => None,
        });
        assert_eq!(v, Some(valeur), "{f}");
    }
}

#[test]
fn firebird_gpc() {
    let Some(fdb) = dossier("FLEXIA_FDB") else {
        return;
    };
    let db = Database::open(fdb.join("GPC.FDB")).unwrap();
    assert_eq!(db.tables().len(), 27);
    let prot = db.rows("PROTOCOL", true).unwrap();
    let ligne = |id: i64| {
        prot.iter()
            .find(|r| col(r, "PROTID").and_then(|v| v.as_i64()) == Some(id))
            .unwrap()
    };

    // blob de niveau 1
    let b = col(ligne(260), "PROTFILE3").and_then(|v| v.as_bytes()).unwrap();
    assert!(b.len() == 7515 && b.starts_with(b"P55011"));

    // P_INJFIG : services KWP par Code Com
    let b = col(ligne(60), "PROTFILE3").and_then(|v| v.as_bytes()).unwrap();
    let (cm, _) = descr::commandes_68k(&descr::parse_68k(b));
    assert_eq!(cm["01"].d, ["E81"]);
    assert_eq!(cm["07"].t, ["E10"]);
    assert_eq!(
        (cm["07"].d[0].as_str(), cm["0C"].d[0].as_str(), cm["0E"].d[0].as_str()),
        ("E21", "E17", "E14")
    );

    // tous les blobs P_*.pro de la base suivent la grammaire .pro
    let mut n = 0;
    for r in &prot {
        for i in 1..=5 {
            let nom = col(r, &format!("PROTFNAME{i}")).and_then(|v| v.as_str()).unwrap_or("");
            let Some(b) = col(r, &format!("PROTFILE{i}")).and_then(|v| v.as_bytes()) else {
                continue;
            };
            if nom.to_ascii_uppercase().starts_with("P_") && nom.to_ascii_lowercase().ends_with(".pro") {
                n += 1;
                assert!(
                    !descr::parse_pro(b)
                        .unwrap()
                        .iter()
                        .any(|x| matches!(x, ProRec::Inconnu { .. })),
                    "{nom}"
                );
            }
        }
    }
    assert!(n > 0);
}

#[test]
fn gpc_injection_s2000pm1() {
    use psa_data::gpc::Gpc;
    let Some(fdb) = dossier("FLEXIA_FDB") else {
        return;
    };
    let g = Gpc::open(fdb.join("GPC.FDB")).unwrap();
    assert_eq!((g.ecu_types.len(), g.commands.len()), (726, 16565));

    let t = g.ecu_type(356).unwrap();
    assert_eq!(
        (t.name.as_str(), t.marque.as_str(), t.prot_id),
        ("S2000PM1", "SAGEM", Some(60))
    );
    let p = g.protocol_of(356).unwrap();
    assert!(p.files.iter().any(|(n, b)| n == "P_INJFIG.68K" && !b.is_empty()));

    // Code cal 8e, Codes Com vus dans la capture du 2026-11-13
    let c = |n: &str| g.command(356, n).unwrap();
    assert!(g.commands_of(356).all(|c| c.code_cal == 0x8e));
    assert_eq!(g.commands_of(356).count(), 19);
    for (n, com) in [
        ("INIT", 1),
        ("FIN_DIAG", 2),
        ("TP", 3),
        ("IDENTIFICATION", 4),
        ("GENERIQUE_HN", 6),
        ("LEC_TRM", 7),
    ] {
        assert_eq!(c(n).code_com, com, "{n}");
    }
    assert_eq!(
        (c("LECTURE_DEFAUTS").code_com, c("LECTURE_DEFAUTS").data.as_slice()),
        (0x0c, &[0xff, 0x00][..])
    );
    assert_eq!(c("EFFACE_DEFAUTS").code_com, 0x0e);

    // véhicule -> famille INJ (cible 10) -> type 356
    let fam = g.families.iter().find(|f| f.id == 47).unwrap();
    assert_eq!((fam.name.as_str(), fam.target.as_str()), ("INJ", "10"));
    assert!(g
        .ecus_of_vehicule(fam.veh_id)
        .iter()
        .any(|(f, _, t)| f.id == 47 && t.id == 356));
}

/// `FLEXIA_MESURES` : dictionnaire TSV produit par `scripts/dico_mesures.py`.
#[test]
fn mesures_injection_ralenti() {
    use psa_data::mesures::Dictionnaire;
    let Some(tsv) = dossier("FLEXIA_MESURES") else {
        return;
    };
    let d = Dictionnaire::open(tsv).unwrap();
    assert_eq!(d.trames.iter().map(|t| t.fiches.len()).sum::<usize>(), 870);
    // réponse capturée le 2026-11-13 à 18:42:17 (ralenti) : régime, tension batterie, température d'eau
    let t = d.trame(356, &[0xc2, 0x80, 0x01]).unwrap();
    let rep = [
        0x01, 0x10, 0x0f, 0x00, 0x05, 0x61, 0xff, 0x1e, 0x8e, 0x47, 0x01, 0x01, 0x00, 0x00, 0x23, 0x18, 0x00, 0x00,
        0x01, 0x00,
    ];
    let v = t.decode(&rep);
    let val = |n: &str| (v.iter().find(|(k, _)| *k == n).unwrap().1.unwrap() * 1e4).round() / 1e4;
    assert_eq!(
        (val("REGIMEMOTEUR"), val("TENSIONBATTERIE"), val("TEMPERATUREEAU")),
        (960.0, 14.2, 31.0)
    );
    assert_eq!(v.len(), 13);
}
