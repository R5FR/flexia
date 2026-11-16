//! Rejeu strict de la session du 2026-11-13 sur l'injection S2000PM1 (type 356) : la session doit
//! émettre exactement les requêtes de Lexia, dans l'ordre, et décoder les valeurs vues à l'écran.
//! Ignoré sans `FLEXIA_FDB`, `FLEXIA_MESURES` et `FLEXIA_CAPTURE` (`echanges_182231.jsonl`).

use diag_core::{garde::Refus, Error, Session};
use psa_data::gpc::Gpc;
use psa_data::mesures::Dictionnaire;
use std::path::PathBuf;
use vci::replay::{Mode, Replay};
use vci::Transport;

const S2000PM1: i64 = 356;

fn env(var: &str) -> Option<PathBuf> {
    let d = std::env::var_os(var).map(PathBuf::from);
    if d.is_none() {
        eprintln!("ignoré : {var} non défini");
    }
    d
}

fn donnees() -> Option<(Gpc, Dictionnaire, Replay)> {
    let (fdb, tsv, cap) = (env("FLEXIA_FDB")?, env("FLEXIA_MESURES")?, env("FLEXIA_CAPTURE")?);
    Some((
        Gpc::open(fdb.join("GPC.FDB")).unwrap(),
        Dictionnaire::open(tsv).unwrap(),
        Replay::open(cap, Mode::Strict).unwrap(),
    ))
}

#[test]
fn session_injection() {
    let Some((gpc, dico, mut rejeu)) = donnees() else {
        return;
    };
    assert_eq!(rejeu.echanges().len(), 2495);
    // action C « Défauts moteur » : statut, C_injfig, P_INJFIG, INIT, IDENTIFICATION
    let debut = rejeu.aller_a_heure("18:30:40").unwrap();
    let mut s = Session::new(rejeu, &gpc);
    let canal = s.ouvrir(S2000PM1).unwrap();
    assert_eq!(canal.code_cal, 0x8e);
    let id = s.identification(&canal).unwrap();
    assert_eq!((id.adresse(), &id.kwp()[..2]), (0x10, &[0x61, 0x80][..]));
    assert_eq!(s.transport().position(), debut + 5);

    // défauts : 17 FF 00 -> P0420, statut 28
    s.transport_mut().aller_a_heure("18:39:26").unwrap();
    s.statut().unwrap();
    let d = s.lire_defauts(&canal).unwrap();
    assert_eq!(
        d.iter().map(|d| (d.code.as_str(), d.statut)).collect::<Vec<_>>(),
        [("P0420", 0x28)]
    );

    // mesures au ralenti à 18:42:17, comme à l'écran de Lexia
    let (c2, c0) = (
        dico.trame(S2000PM1, &[0xc2, 0x80, 0x01]).unwrap(),
        dico.trame(S2000PM1, &[0xc0, 0x80, 0x01]).unwrap(),
    );
    s.transport_mut().aller_a_heure("18:42:17").unwrap();
    let mut v = Vec::new();
    // la capture place le statut avant ou après la paire de trames selon l'instant : on se cale sur c2
    if s.transport().echanges()[s.transport().position()].req.code_cal == 0 {
        s.statut().unwrap();
    }
    v.extend(s.lire_mesures(&canal, c2).unwrap());
    v.extend(s.lire_mesures(&canal, c0).unwrap());
    let val = |n: &str| (v.iter().find(|(k, _)| *k == n).unwrap().1.unwrap() * 1e4).round() / 1e4;
    assert_eq!(
        (
            val("REGIMEMOTEUR"),
            val("TENSIONBATTERIE"),
            val("TEMPERATUREEAU"),
            val("TENSIONSONDEAMONT"),
            val("TEMPSINJECTION")
        ),
        (992.0, 14.2, 31.0, 0.6435, 4.92)
    );

    // tout le relevé « paramètres » (D1 + D2) : mêmes requêtes que Lexia, toutes décodées
    let i = s.transport_mut().aller_a_heure("18:40:30").unwrap();
    let mut cycles = 0;
    loop {
        let e = &s.transport().echanges()[s.transport().position()];
        match (e.req.code_cal, e.req.code_com, e.req.data.first()) {
            (0x00, 0xfe, _) => drop(s.statut().unwrap()),
            (0x8e, 0x07, Some(0xc2)) => {
                assert_eq!(s.lire_mesures(&canal, c2).unwrap().len(), 13);
                cycles += 1;
            }
            (0x8e, 0x07, Some(0xc0)) => assert!(s.lire_mesures(&canal, c0).unwrap().iter().all(|(_, v)| v.is_some())),
            _ => break,
        }
    }
    assert_eq!(cycles, 470);
    assert!(s.transport().position() - i > 1390);

    // fin : FIN_DIAG
    assert_eq!(s.transport().echanges()[s.transport().position()].heure, "18:44:19.671");
    s.fermer(canal).unwrap();
}

#[test]
fn ecritures_refusees_sans_envoi() {
    let Some((gpc, _, mut rejeu)) = donnees() else {
        return;
    };
    rejeu.aller_a_heure("18:30:40").unwrap();
    let mut s = Session::new(rejeu, &gpc);
    let canal = s.ouvrir(S2000PM1).unwrap();
    let avant = s.transport().position();
    for nom in [
        "EFFACE_DEFAUTS",
        "RAZ_CHGT_ETATS",
        "EFFACEMENT",
        "TELECODAGE",
        "PILOTAGE",
        "RESET_ECU",
        "REQDWN",
    ] {
        assert!(
            matches!(s.commande(&canal, nom, &[0]), Err(Error::Refus(Refus::Nom(_)))),
            "{nom}"
        );
    }
    // services d'écriture par la commande générique
    for srv in [0x14u8, 0x2E, 0x3B, 0x30, 0x31, 0x34, 0x36, 0x27, 0x11, 0x10] {
        assert!(
            matches!(s.commande(&canal, "GENERIQUE_HN", &[srv, 0]), Err(Error::Refus(Refus::Service(x))) if x == srv)
        );
    }
    // DEFAUTS_FIGES passe par le service 12 : non autorisé
    assert!(matches!(
        s.commande(&canal, "DEFAUTS_FIGES", &[0]),
        Err(Error::Refus(Refus::Service(0x12)))
    ));
    assert!(matches!(s.systeme(0x06, &[]), Err(Error::Refus(Refus::Systeme(0x06)))));
    assert_eq!(s.transport().position(), avant, "rien ne doit avoir été envoyé");
    // le transport lui-même n'a rien vu : la prochaine requête attendue est toujours l'identification
    assert!(s.transport_mut().exchange(&vci::Request::new(0x8e, 0x04, [])).is_ok());
}

/// Toutes les commandes d'écriture de la base sont refusées par leur nom, pour tous les calculateurs.
#[test]
fn ecritures_de_la_base_refusees() {
    let Some(fdb) = env("FLEXIA_FDB") else {
        return;
    };
    let gpc = Gpc::open(fdb.join("GPC.FDB")).unwrap();
    let mut n = 0;
    for c in gpc
        .commands
        .iter()
        .filter(|c| diag_core::garde::NOMS_REFUSES.contains(&c.name.as_str()))
    {
        assert!(matches!(
            diag_core::garde::commande(&c.name, None, &c.data),
            Err(Refus::Nom(_))
        ));
        n += 1;
    }
    assert!(n > 2500, "{n}");
}

/// Phase de connexion : réinitialisation, démarrage du firmware, statut, version, voltmètre.
#[test]
fn connexion_vci() {
    let Some((gpc, _, mut rejeu)) = donnees() else {
        return;
    };
    rejeu.aller_a(2);
    let mut s = Session::new(rejeu, &gpc);
    assert_eq!(s.connecter().unwrap().version(), Some("V4.3.2"));
    assert_eq!(s.connecter().unwrap().champs.len(), 5);
    assert_eq!(s.statut().unwrap(), [0, 0]);
    assert_eq!(s.version().unwrap().version(), Some("V4.3.2"));
    s.transport_mut().aller_a(13);
    s.statut().unwrap();
    assert_eq!(s.voltmetre().unwrap(), 0x0b56);
}
