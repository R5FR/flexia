# flexia

Outil de diagnostic **64 bits** pour Citroën / PSA (C5 phase 1, interface PP2000), écrit en Rust, qui remplace Lexia/DiagBox sans VM.

Repo principal : il contiendra le cœur (lecture des données PSA, transport VCI, session de diagnostic) et regroupera les autres repos.

| Repo | Rôle |
|---|---|
| `flexia` | cœur Rust : `psa-data`, `vci`, `diag-core` |
| [`flexia-cli`](https://github.com/R5FR/flexia-cli) | ligne de commande |
| [`flexia-api`](https://github.com/R5FR/flexia-api) | API HTTP locale (127.0.0.1) |
| [`flexia-mcp`](https://github.com/R5FR/flexia-mcp) | serveur MCP |
| [`flexia-data`](https://github.com/R5FR/flexia-data) | rapports et données de l'analyse |

Plan détaillé : `11_plan_rust.md` dans `flexia-data`. Lecture seule par défaut ; toute écriture est déverrouillée opération par opération.

État : **étapes 1 et 2 faites**.

## État
Étapes 1 et 2 du plan faites (sans matériel). Aucune donnée PSA n'est dans ce dépôt : tout est lu dans la copie du pack au lancement.

| Crate | Rôle |
|---|---|
| `psa-data` | lecture seule des données du pack : Firebird ODS 10, base véhicule `GPC`, dictionnaires `.DBZ`/`.ZCL`, descripteurs `SUPER.PC` / `.68K` / `.pro`, tables de mesures |
| `vci` | transport vers l'interface : trait `Transport`, rejeu d'une capture (le backend Windows reste à écrire) |
| `diag-core` | session de diagnostic, garde lecture seule, décodage des défauts et des mesures |
| `apps/flexia-cli` (sous-module) | ligne de commande `flexia` |
| `apps/flexia-api` (sous-module) | API HTTP locale et interface web `flexia-api` |

```sh
cargo test                       # tests unitaires
# tests sur les données réelles : définir FLEXIA_LEXIA, FLEXIA_COMMON, FLEXIA_FDB, FLEXIA_MESURES, FLEXIA_CAPTURE puis
cargo test --release
# test croisé avec les parseurs Python de référence (sorties identiques attendues)
cargo run --release -p psa-data --example digest -- --lexia … --common … --fdb … > rust.txt
python -I tools/crosscheck.py --parsers …/parsers --lexia … --common … --fdb … > py.txt
```
