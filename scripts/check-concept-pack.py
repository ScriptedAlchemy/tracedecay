#!/usr/bin/env python3
"""Validate final design handoffs; optionally compare a split export's plates."""
import argparse
from collections import Counter
from pathlib import Path
import re
import subprocess
from urllib.parse import unquote, urlsplit


ROOT = Path(__file__).resolve().parents[1]
CANONICAL = ROOT / 'mockups/ui-concept-v2'


def links(path):
    # The pack uses inline Markdown links, including images and table cells.
    text = re.sub(r'```.*?```', '', path.read_text(), flags=re.S)
    return re.findall(r'\]\(([^\s)]+)\)', text)


def validate(pack):
    errors = []
    image_root = pack / 'pngs' if (pack / 'pngs').is_dir() else pack
    brief_root = pack / 'briefs' if (pack / 'briefs').is_dir() else pack
    rail = re.findall(r'^\| (\d{2}) \| (\w+) \|', (CANONICAL / 'NAVIGATION.md').read_text(), re.M)
    surfaces = [f'{number}-{name.lower()}' for number, name in rail]
    for root in {image_root, brief_root}:
        actual = sorted(p.name for p in root.iterdir() if p.is_dir() and re.match(r'\d{2}-', p.name))
        if actual != surfaces:
            errors.append(f'{root}: surface order/names differ from canonical navigation')
    if re.findall(r'^\| (\d{2}) \| (\w+) \|', (pack / 'NAVIGATION.md').read_text(), re.M) != rail:
        errors.append(f'{pack}: navigation differs from canonical rail')
    plates = {}
    for surface in surfaces:
        images = image_root / surface / 'final'
        briefs = brief_root / surface / 'final'
        manifest = briefs / 'README.md'
        if not manifest.is_file():
            errors.append(f'{manifest}: missing final manifest')
            continue
        counts = Counter(unquote(urlsplit(link).path) for link in links(manifest))
        pngs = {p.stem: p for p in images.glob('*.png')}
        mds = {p.stem: p for p in briefs.glob('*.md') if p.name != 'README.md'}
        if not pngs:
            errors.append(f'{images}: no final plates')
        for stem in pngs.keys() - mds.keys():
            errors.append(f'{pngs[stem]}: missing same-stem brief')
        for stem in mds.keys() - pngs.keys():
            errors.append(f'{mds[stem]}: orphaned brief')
        for stem, png in pngs.items():
            plates[f'{surface}/final/{png.name}'] = png
            for target in (png, briefs / f'{stem}.md'):
                count = sum(n for link, n in counts.items() if (manifest.parent / link).resolve() == target.resolve())
                if count != 1:
                    errors.append(f'{manifest}: {target.name} has {count} mappings; expected one')
    link_roots = [pack.resolve()]
    if image_root != pack:
        # The split export links its separately labelled application screenshots.
        link_roots.append((pack.parent / 'screenshots').resolve())
    for doc in pack.rglob('*.md'):
        for link in links(doc):
            parsed = urlsplit(link)
            if parsed.scheme or parsed.netloc or not parsed.path:
                continue
            target = (doc.parent / unquote(parsed.path)).resolve()
            if not any(target.is_relative_to(root) for root in link_roots) or not target.exists():
                errors.append(f'{doc}: broken or escaping local link {link}')
    return plates, errors


def provenance(pack, plates):
    revision = subprocess.check_output(['git', '-C', str(pack), 'rev-parse', 'HEAD'], text=True).strip()
    print(f'{pack}: source commit {revision}; {len(plates)} final plates')
    for name, path in sorted(plates.items()):
        blob = subprocess.check_output(['git', 'hash-object', str(path)], text=True).strip()
        print(f'{blob} {name}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('pack', nargs='?', type=Path, default=CANONICAL)
    parser.add_argument('--mirror', type=Path, help='Validate a second pack and compare its final PNG bytes')
    args = parser.parse_args()
    plates, errors = validate(args.pack)
    provenance(args.pack, plates)
    if args.mirror:
        mirrored, mirror_errors = validate(args.mirror)
        errors.extend(mirror_errors)
        provenance(args.mirror, mirrored)
        if plates.keys() != mirrored.keys():
            errors.append('mirror: final plate names differ')
        for name in plates.keys() & mirrored.keys():
            if plates[name].read_bytes() != mirrored[name].read_bytes():
                errors.append(f'mirror: plate bytes differ: {name}')
    for error in errors:
        print(error)
    return bool(errors)


if __name__ == '__main__':
    raise SystemExit(main())
