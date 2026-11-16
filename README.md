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

État : **étape 1 en cours**.

## État
`crates/psa-data` : lecture seule des données du pack (Firebird ODS 10, dictionnaires `.DBZ`/`.ZCL`, descripteurs `SUPER.PC` / `.68K` / `.pro`). Aucune donnée PSA n'est dans ce dépôt.

```sh
cargo test                       # tests unitaires
# tests sur le pack : définir FLEXIA_LEXIA, FLEXIA_COMMON, FLEXIA_FDB puis
cargo test --release
# test croisé avec les parseurs Python de référence (sorties identiques attendues)
cargo run --release -p psa-data --example digest -- --lexia … --common … --fdb … > rust.txt
python -I tools/crosscheck.py --parsers …/parsers --lexia … --common … --fdb … > py.txt
```
