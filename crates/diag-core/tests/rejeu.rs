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
    // mêmes valeurs que l'écran « identification » de Lexia
    let champs = diag_core::decode_identification(id.kwp()).unwrap();
    assert_eq!(
        (
            champs.reference_psa.as_str(),
            champs.reference_complementaire.as_deref().unwrap(),
            champs.version_diagnostic.as_str()
        ),
        ("9644674580", "9655212680", "0D")
    );
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
    assert!(s.fermer(canal).unwrap(), "statut 00 00 attendu après FIN_DIAG");
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
    for c in gpc.commands.iter().filter(|c| diag_core::garde::nom_refuse(&c.name)) {
        assert!(matches!(
            diag_core::garde::commande(&c.name, None, &c.data),
            Err(Refus::Nom(_))
        ));
        n += 1;
    }
    assert!(n > 3500, "{n}");
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
    assert_eq!(s.voltmetre().unwrap(), 11.86, "0b 56 = 11 V + 86 centièmes");
}

/// BSI de la C5 (type 264, canal 7a) : ouverture comme Lexia (INIT par trame complète), lecture de la
/// zone b0, qui contient le VIN (déjà masqué dans la capture enregistrée).
#[test]
fn bsi_ouverture_et_zone_vin() {
    let Some((gpc, _, mut rejeu)) = donnees() else {
        return;
    };
    const BSI_X4: i64 = 264;
    rejeu.aller_a(21);
    let mut s = Session::new(rejeu, &gpc);
    let canal = s.ouvrir(BSI_X4).unwrap();
    assert_eq!(
        (canal.code_cal, s.transport().position()),
        (0x7a, 26),
        "fe, C_, P_ en deux blocs, INIT"
    );
    assert_eq!(s.statut().unwrap(), [1, 0]);
    let zone = s.commande(&canal, "IDENTIFICATION_1", &[]).unwrap();
    assert_eq!((zone.adresse(), &zone.kwp()[..2]), (0xd0, &[0x61, 0xb0][..]));
    let masque = vci::masquer_vin(zone.kwp());
    // la capture enregistrée a déjà son VIN masqué : le masquage ne doit rien y changer
    assert_eq!(masque, zone.kwp());
    assert_eq!(zone.kwp().len(), 19, "61 b0 + 17 caractères");
    assert_eq!(&masque[5..15], b"XXXXXXXXXX");
    assert_eq!(masque.len(), zone.kwp().len());
    // écritures de la BSI refusées sans envoi
    let avant = s.transport().position();
    for nom in [
        "TELECODAGE",
        "TELECODAGE1",
        "ACTIVATION",
        "EFFACE_DEFAUTS",
        "RESET_ECU",
        "TELECHARGEMENT",
    ] {
        assert!(
            matches!(s.commande(&canal, nom, &[0]), Err(Error::Refus(Refus::Nom(_)))),
            "{nom}"
        );
    }
    // MAPPING (service 35, lecture) n'est permise que sous la forme « famille 00 00 bloc »
    for nom in ["WRITE_MODE_FONC", "ECR_PROTECT", "ECR_CONFIG", "MAPPING"] {
        assert!(matches!(s.commande(&canal, nom, &[0]), Err(Error::Refus(_))), "{nom}");
    }
    assert_eq!(s.transport().position(), avant);
}

/// BSI de la C5 comme Lexia au test global du 2026-11-13 (session de 18:27:54) : ouverture, composition des
/// zones téléchargée (`MAPPING`, 31 échanges), identification, zones b1, b2, b0 ; puis lecture des zones de
/// défauts de la session précédente (83 et 88 refusées par la BSI, 84 et 87 à zéro) et fin.
#[test]
fn bsi_composition_zones_et_defauts() {
    let Some((gpc, _, mut rejeu)) = donnees() else {
        return;
    };
    let Some(lexia) = env("FLEXIA_LEXIA") else {
        return;
    };
    let ini = psa_data::bsi::Ini::charger(lexia.join("DATA").join("BSI.ini")).unwrap();
    let debut = rejeu.aller_a_heure("18:27:54.40").unwrap();
    let mut s = Session::new(rejeu, &gpc);
    let bsi = s.ouvrir(psa_data::bsi::TYPE_BSI_X4).unwrap();
    assert_eq!(s.transport().position(), debut + 5, "fe, C_, P_ en deux blocs, INIT");
    let (comp, reponses) = s.composition_bsi(&bsi, &psa_data::bsi::FAMILLES).unwrap();
    assert_eq!(reponses.len(), 31, "C0 ×9, B0 ×7, D0, A0, 83 ×13");
    let zones: Vec<u8> = comp.zones().collect();
    for z in [0x84, 0x86, 0x87, 0xb0, 0xb1, 0xb2, 0xc9, 0xca, 0xd0] {
        assert!(zones.contains(&z), "zone {z:02x}");
    }
    assert!(!comp.contient(0x83) && !comp.contient(0x88), "zones refusées par cette BSI");
    let taille = |z: u8| comp.taille(z, &ini).unwrap();
    assert_eq!((taille(0x84), taille(0x87), taille(0xb1), taille(0xb2), taille(0xb0)), (13, 1, 8, 13, 17));
    s.identification(&bsi).unwrap();
    // zones lues par Lexia après l'identification : tailles conformes à la composition
    let mut lues = Vec::new();
    for z in [0xb1, 0xb2, 0xb0, 0xb1] {
        let d = s.zone_bsi(&bsi, z).unwrap().unwrap();
        assert_eq!(d.len(), taille(z), "zone {z:02x}");
        lues.push((z, d));
    }
    assert_eq!(s.statut().unwrap(), [1, 0]);
    // options de la BSI (zone b2), si le dictionnaire des paramètres est donné
    if let Some(p) = env("FLEXIA_BSI_PARAMETRES") {
        let dico = psa_data::bsi::DictionnaireBsi::charger(p).unwrap();
        let b2 = &lues[1].1;
        let v = dico.valeurs(0xb2, b2, &comp, &ini).unwrap();
        let val = |nom: &str| v.iter().find(|(p, _)| p.nom == nom).map(|(_, x)| x.clone());
        use psa_data::bsi::Valeur::Nombre;
        assert_eq!(val("OPTCLIM"), Some(Nombre(1.0)));
        assert_eq!(val("OPTCAPTEURPLUIE"), Some(Nombre(1.0)));
        assert_eq!(val("OPTBVA"), Some(Nombre(0.0)));
        // zone c9, trame fabriquée : température d'eau 0x7b -> 83 °C (CAL264, conversion 9 : x - 40)
        let champs = comp.champs(0xc9, &ini).unwrap();
        let eau = champs.iter().find(|c| c.rang == 0x24).unwrap();
        let mut c9 = vec![0u8; taille(0xc9)];
        c9[eau.position] = 0x7b;
        let v = dico.valeurs(0xc9, &c9, &comp, &ini).unwrap();
        let eau = v.iter().find(|(p, _)| p.nom == "TEMPEAU").map(|(_, x)| x.clone());
        assert_eq!(eau, Some(Nombre(83.0)));
    }
    // zones de défauts, lues dans la session précédente (même BSI, même composition)
    s.transport_mut().aller_a_heure("18:27:54.11").unwrap();
    let defauts = match env("FLEXIA_BSI_DEFAUTS") {
        Some(p) => psa_data::bsi::DefautsBsi::charger(p).unwrap(),
        None => psa_data::bsi::DefautsBsi::default(),
    };
    let (d, trames) = s.lire_defauts_bsi(&bsi, &comp, &ini, &defauts).unwrap();
    assert!(d.is_empty(), "aucun défaut BSI le 13/11 : {d:?}");
    assert_eq!(trames.keys().copied().collect::<Vec<_>>(), [0x84, 0x87]);
    assert_eq!(trames[&0x84], vec![0; 13]);
    if !defauts.is_empty() {
        // bit d'indice 16 de la zone 84 = rang c9 dans cette composition -> 2CZ (CAL264, en émulation)
        let mut t = trames.clone();
        t.get_mut(&0x84).unwrap()[2] = 0x01;
        let d = defauts.decoder(264, &t, &comp, &ini).unwrap();
        assert_eq!(d.iter().map(|d| (d.code.as_str(), d.rang)).collect::<Vec<_>>(), [("2CZ", 0xc9)]);
        assert_eq!(defauts.table_libelles(264, "CITROENC5"), Some("MENU906N"));
    }
    assert!(s.fermer(bsi).unwrap(), "FIN_DIAG de la BSI, puis statut 00 00");
    assert_eq!(s.transport().echanges()[s.transport().position() - 1].heure, "18:27:54.400");
}

/// Écran « identification » de Lexia du 2026-11-13 à 18:31 : mêmes trois lectures, mêmes sept valeurs.
#[test]
fn identification_comme_lexia() {
    let Some((gpc, _, mut rejeu)) = donnees() else {
        return;
    };
    rejeu.aller_a_heure("18:30:40").unwrap();
    let mut s = Session::new(rejeu, &gpc);
    let canal = s.ouvrir(S2000PM1).unwrap();
    s.transport_mut().aller_a_heure("18:31:25.82").unwrap();
    let id = s.identification_complete(&canal).unwrap();
    let champs: Vec<(&str, &str)> = id.champs.iter().map(|(n, v)| (*n, v.as_str())).collect();
    assert_eq!(
        champs,
        [
            ("FOURNISSEUR", "5"),
            ("REF_PSA", "9644674580"),
            ("REFERENCE_SOFT", "9655212680"),
            ("REF_LOG", "9616161680"),
            ("EDITIONSOFT", "1400"),
            ("DATEDERINTERV", "20/09/2026"),
            ("VERSION_DIAG", "0D"),
        ]
    );
    // la BSI n'est pas une injection S2000 : pas de lecture supplémentaire
    assert!(diag_core::decode_zone_fe(id.reponse.kwp()).is_none());
}

/// ABS MK60 de la C5 (type 233, canal 54, adresse 20) : même séquence que le test global de Lexia.
#[test]
fn abs_identification_et_defauts() {
    let Some((gpc, _, mut rejeu)) = donnees() else {
        return;
    };
    const ABS_MK60: i64 = 233;
    let debut = rejeu.aller_a_heure("18:28:41.30").unwrap();
    let mut s = Session::new(rejeu, &gpc);
    let canal = s.ouvrir(ABS_MK60).unwrap();
    assert_eq!(canal.code_cal, 0x54);
    let id = s.identification_complete(&canal).unwrap();
    assert_eq!(id.reponse.adresse(), 0x20);
    let champs: Vec<(&str, &str)> = id.champs.iter().map(|(n, v)| (*n, v.as_str())).collect();
    assert_eq!(
        champs,
        [("FOURNISSEUR", "9"), ("REF_PSA", "9641767380"), ("VERSION_DIAG", "0B")]
    );
    assert_eq!(s.statut().unwrap(), [1, 0]);
    assert!(s.lire_defauts(&canal).unwrap().is_empty(), "57 00 : aucun défaut");
    assert!(s.fermer(canal).unwrap());
    assert_eq!(s.transport().position(), debut + 9);
}

/// Test global du 13/11 : combiné (246), climatisation (270) et afficheur (319) joints par la BSI, dont la
/// session reste ouverte, avec exactement les échanges de Lexia : statut, descripteur `P_` du calculateur
/// (la VCI répond par le nombre d'enregistrements chargés), identification, statut, lecture des défauts,
/// `FIN_DIAG` ; enfin `FIN_DIAG` de la BSI. Défauts décodés par les tables des modules (`FLEXIA_DEFAUTS`).
#[test]
fn habitacle_par_la_bsi() {
    let Some((gpc, _, mut rejeu)) = donnees() else {
        return;
    };
    let Some(tsv) = env("FLEXIA_DEFAUTS") else {
        return;
    };
    let dico = psa_data::defauts::DictionnaireDefauts::charger(tsv).unwrap();
    const BSI_X4: i64 = 264;
    let debut = rejeu.aller_a_heure("18:27:54.40").unwrap();
    let mut s = Session::new(rejeu, &gpc);
    let bsi = s.ouvrir(BSI_X4).unwrap();
    assert_eq!(s.transport().position(), debut + 5, "fe, C_, P_ en deux blocs, INIT");
    // Lexia lit alors les zones de la BSI ; reprise au chargement du descripteur du combiné
    s.transport_mut().aller_a_heure("18:27:58.55").unwrap();
    let lire = |s: &mut Session<'_, Replay>, id: i64| -> (u8, Vec<String>) {
        let canal = s.joindre(&bsi, id).unwrap();
        let ident = s.identification(&canal).unwrap();
        assert_eq!(s.statut().unwrap(), [1, 0]);
        let table = dico.table(id, Some("CITROENC5")).unwrap();
        let (_, d) = s.lire_defauts_bits(&canal, table).unwrap();
        assert!(d.iter().all(|d| d.methode));
        s.quitter(canal).unwrap();
        (ident.adresse(), d.into_iter().map(|d| d.code).collect())
    };
    // combiné : 61 c3 00 00 00 00 e0 00 -> trois défauts fugitifs
    assert_eq!(
        lire(&mut s, 246),
        (0x5b, vec!["H7X".into(), "H8X".into(), "H9X".into()])
    );
    assert_eq!(
        s.statut().unwrap(),
        [1, 0],
        "Lexia relit le statut avant le calculateur suivant"
    );
    // climatisation : 61 c4 00×9 80 -> un défaut fugitif (bit 80 de l'octet 11)
    assert_eq!(lire(&mut s, 270), (0x31, vec!["HAA".into()]));
    // afficheur : seconde passe de Lexia (la première ne lit que l'identification)
    s.transport_mut().aller_a_heure("18:28:18.82").unwrap();
    assert_eq!(lire(&mut s, 319), (0xb5, vec!["0GZ".into()]));
    assert!(s.fermer(bsi).unwrap(), "FIN_DIAG de la BSI (7a 02), puis statut 00 00");
    let fin = s.transport().position();
    assert_eq!(s.transport().echanges()[fin - 1].heure, "18:28:19.943");

    // libellés : table du menu « Lecture défauts » de chaque calculateur, phrases des dictionnaires
    let Some(lexia) = env("FLEXIA_LEXIA") else {
        return;
    };
    let dicos = psa_data::libelles::Dicos::new(lexia.join("DICO"), "FR");
    let libelle = |id: i64, code: &str| {
        let table = dico.table_libelles(id, "CITROENC5").unwrap();
        let phrases =
            psa_data::dbf::libelles_defauts(lexia.join("SGML").join("Menu").join(format!("{table}.DBF"))).unwrap();
        dicos.libelle(&phrases[code]).to_string()
    };
    assert_eq!(
        libelle(246, "H7X"),
        "défaut fugitif — Régime moteur : valeur reçue incorrecte"
    );
    assert_eq!(
        libelle(246, "H9X"),
        "défaut fugitif — Vitesse véhicule : valeur reçue incorrecte"
    );
    assert_eq!(libelle(270, "HAA"), "défaut fugitif — Pulseur : Circuit ouvert");
    assert_eq!(
        libelle(319, "0GZ"),
        "défaut permanent — Absence de communication avec l'autoradio"
    );
}

/// Calculateur d'habitacle dont la BSI garde les défauts, comme le test global du 2026-11-13 à 18:28:05 : joint par
/// la BSI, composition téléchargée par sa propre commande `MAPPING` (familles C0 et 83), identification, puis comme
/// son `LECTDEF` : fin de dialogue, zones 85 (refusée) et 86 de la BSI par `LEC_TRM_BSI`, nouvelle identification ;
/// enfin fin de dialogue. Aucun défaut ce jour-là.
#[test]
fn defauts_gardes_par_la_bsi() {
    let Some((gpc, _, mut rejeu)) = donnees() else {
        return;
    };
    let (Some(lexia), Some(p)) = (env("FLEXIA_LEXIA"), env("FLEXIA_BSI_DEFAUTS")) else {
        return;
    };
    let ini = psa_data::bsi::Ini::charger(lexia.join("DATA").join("BSI.ini")).unwrap();
    let defauts = psa_data::bsi::DefautsBsi::charger(p).unwrap();
    rejeu.aller_a_heure("18:27:54.40").unwrap();
    let mut s = Session::new(rejeu, &gpc);
    let bsi = s.ouvrir(psa_data::bsi::TYPE_BSI_X4).unwrap();
    // le calculateur joint à 18:28:05 : le type dont le descripteur P_ est celui envoyé à la VCI
    let mut joint = None;
    for t in [359, 227, 225, 226] {
        s.transport_mut().aller_a_heure("18:28:05.32").unwrap();
        if let Ok(c) = s.joindre(&bsi, t) {
            joint = Some(c);
            break;
        }
    }
    let canal = joint.expect("un des quatre types");
    assert!(defauts.connait(canal.ecu_type_id));
    let (comp, rep) = s.composition_bsi(&canal, &[0xC0, 0x83]).unwrap();
    assert_eq!(rep.len(), 22, "C0 ×9 et 83 ×13, par la commande MAPPING du calculateur");
    assert!(comp.contient(0x86) && !comp.contient(0x85));
    let ident = s.identification(&canal).unwrap();
    assert_eq!(ident.adresse(), 0x45);
    assert_eq!(s.statut().unwrap(), [1, 0]);
    let (d, trames) = s.lire_defauts_par_bsi(&canal, &comp, &ini, &defauts).unwrap();
    assert!(d.is_empty(), "{d:?}");
    assert_eq!(trames.keys().copied().collect::<Vec<_>>(), [0x86], "85 refusée par la BSI");
    assert_eq!(trames[&0x86].len(), comp.taille(0x86, &ini).unwrap());
    s.quitter(canal).unwrap();
    assert_eq!(s.statut().unwrap(), [1, 0]);
    assert_eq!(s.transport().echanges()[s.transport().position() - 1].heure, "18:28:08.433");
}
