#!/usr/bin/env python3
"""Empreintes des données lues par les parseurs Python de référence, au même format que
`cargo run --release -p psa-data --example digest`. Les deux sorties doivent être identiques.

Usage : python -I crosscheck.py --parsers <dossier parsers> [--lexia <APP/LEXIA>] [--common <APP/COMMON>] [--fdb <dossier>]
"""
import hashlib, os, struct, sys

sys.stdout.reconfigure(encoding='utf-8', newline='\n')
arg = lambda k: sys.argv[sys.argv.index(k) + 1] if k in sys.argv else None
sys.path.insert(0, arg('--parsers'))
import dico, ods10, vci_descr

BASES = ['DSD', 'GENERATIONDICOV2', 'GPC', 'QUICKDEF']
lignes = []


def fichiers(d, exts):
    for root, _, fs in os.walk(d):
        for f in fs:
            if os.path.splitext(f)[1].lower().lstrip('.') in exts:
                yield os.path.join(root, f)


def rel(p, base):
    return os.path.relpath(p, base).replace(os.sep, '/')


def canon(v):
    if v is None:
        return b'\\N'
    if isinstance(v, float):
        return struct.pack('<d', v).hex().encode()
    if isinstance(v, int):
        return str(v).encode()
    if isinstance(v, bytes):
        return b'b:' + v.hex().encode()
    return b's:' + v.encode('utf-8')


if arg('--fdb'):
    for base in BASES:
        db = ods10.Database(os.path.join(arg('--fdb'), base + '.FDB'))
        for t in db.tables():
            h, n = hashlib.sha256(), 0
            for row in db.rows(t, blobs=True):
                n += 1
                for k, v in row.items():
                    h.update(k.encode() + b'=' + canon(v) + b'\x1f')
                h.update(b'\x1e')
            lignes.append(f'fdb\t{base}/{t}\t{n}\t{h.hexdigest()}')

if arg('--lexia'):
    d0 = os.path.join(arg('--lexia'), 'DICO')
    for p in fichiers(d0, ('dbz', 'zcl')):
        d = dico.Dico(p)
        hr, ht = hashlib.sha256(), hashlib.sha256()
        for i in range(len(d)):
            hr.update(d.raw(i) + b'\0')
            ht.update(d[i].encode('utf-8') + b'\n')
        c = d.check()
        drapeaux = ''.join(str(int(c[k])) for k in ('monotone', 'first_at_data_offset', 'in_bounds', 'last_string_ends_at_eof'))
        lignes.append(f"dico\t{rel(p, d0)}\t{d.kind}:{d.name or '-'}:{d.count}:{drapeaux}:{c['empty']}\t{hr.hexdigest()}\t{ht.hexdigest()}")

if arg('--common'):
    com = arg('--common')
    for p in fichiers(com, ('pc', '68k', 'pro')):
        ext = os.path.splitext(p)[1].lower().lstrip('.')
        s = ''
        if ext == 'pc':
            rows = vci_descr.parse_pc(p)
            n = len(rows)
            for r in rows:
                s += f"{r['classe']}|{r['code']}|{r['delai']}|{r['libelle']}\n"
        elif ext == '68k':
            recs = vci_descr.parse_68k(p)
            n = len(recs)
            for r in recs:
                l = r.get('ligne')
                s += f"{r['raw']}|{r['id']}|" + (f"{l['start']},{l['data']},{l['stop']},{l['parite']},{l['bauds']}" if l else '') + '\n'
            cmds, divers = vci_descr.commandes_68k(recs)
            for k in sorted(cmds):
                c = cmds[k]
                s += f"{k}:H={','.join(c['H'])};T={','.join(c['T'])};S={','.join(c['S'])};D={','.join(c['D'])};R={int(bool(c['R']))}\n"
            for x in divers:
                s += f'X:{x}\n'
        else:
            recs = vci_descr.parse_pro(p)
            n = len(recs)
            for t, i, v in recs:
                s += f"{t},{i},{'-' if v is None else v}\n"
        lignes.append(f'{ext}\t{rel(p, com)}\t{n}\t{hashlib.sha256(s.encode("utf-8")).hexdigest()}')

print('\n'.join(sorted(lignes)))
