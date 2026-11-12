import argparse
import copy
import hashlib
import pathlib
import sqlite3
import sys
import tempfile

sys.dont_write_bytecode = True
EVIDENCE = pathlib.Path('/tmp/tracedecay-storage-redesign/arena/bakeoff')
sys.path.insert(0, str(EVIDENCE))
import bakeoff as b
import prepare
from s3_plus import S3Plus

ROOT = pathlib.Path(__file__).resolve().parents[1]
SCRATCH = pathlib.Path('/tmp/tracedecay-storage-redesign/interrogate/sol')


def seed(engine, count, initial=None):
    fixture = ROOT / 'crates/tracedecay-code-extraction/fixtures/cross-file-calls/python/app'
    work = {}
    for doc, path in enumerate(sorted(fixture.glob('*.py'))[:count]):
        raw = path.read_bytes()
        logical = 'app/' + path.name
        sha = hashlib.sha1(b'blob ' + str(len(raw)).encode() + b'\0' + raw).hexdigest()
        work[doc] = (logical, sha, len(raw), sorted(prepare.keys(raw, logical)))
        engine.pending[doc] = (logical, sha, len(raw), b.Z.compress(raw))
        engine.content[doc] = engine.content_key(logical, sha)
    assert len(work) == count
    ids = b.BitMap(work if initial is None else initial)
    engine.cold(b.postings(ids, work), ids)
    return work


def exact_key(work, doc):
    return b'E' + work[doc][0].encode()


def bound_digest(seg, key, value):
    return hashlib.sha256(seg.to_bytes(8, 'big') + len(key).to_bytes(8, 'big') + key + value).digest()


def held_reader(engine):
    work = seed(engine, 9, initial=[0])
    original = engine.snapshots['main']
    engine.root('unexpired-read-hold', original['parts'], original['members'])
    lease = copy.deepcopy(engine.snapshots['unexpired-read-hold'])
    key = exact_key(work, 0)
    before = list(engine.query(lease, key, engine.c))
    assert before == [0]
    print('held reader before merge =', before)
    inputs = [seg for seg, _ in lease['parts']]
    engine.c.execute('begin immediate')
    for doc in range(1, 9):
        added = b.BitMap([doc])
        members = engine.snapshots['main']['members'] | added
        engine.refresh('main', 'main', b.postings(added, work), b.BitMap(), members)
    engine.reclaim()
    engine.c.execute('commit')
    assert any(merge['reason'] == 'cap' for merge in engine.merge_records)
    print('normal compaction policy triggered = cap')
    print('hold remains rooted =', 'unexpired-read-hold' in engine.snapshots)
    print('catalog view after merge =', list(engine.query(engine.snapshots['unexpired-read-hold'], key, engine.c)))
    after = list(engine.query(lease, key, engine.c))
    assert after == []
    print('held reader after merge =', after)
    print('original input rows remaining =', sum(engine.c.execute('select count(*) from rows where seg=?', (seg,)).fetchone()[0] for seg in inputs))


def read_varint(data, offset):
    value = 0
    for index in range(9):
        byte = data[offset + index]
        if index == 8:
            return (value << 8) | byte, offset + index + 1
        value = (value << 7) | (byte & 127)
        if byte < 128:
            return value, offset + index + 1
    raise AssertionError('invalid SQLite varint')


def checked_postings(engine, snap, key):
    result = b.BitMap()
    for seg, alive in snap['parts']:
        row = engine.c.execute('select value,payload_digest,bound_digest from rows where seg=? and key=?', (seg, key)).fetchone()
        if row is not None:
            engine.extra['point_row_checks'] = engine.extra.get('point_row_checks', 0) + 1
            value, digest, bound = row
            assert hashlib.sha256(value).digest() == digest, 'Corrupt'
            assert bound_digest(seg, key, value) == bound, 'Corrupt'
            result.update(engine.segs[seg].docs[i] for i in b.bm(value) & alive)
    return list(result)


def corrupt_lookup(engine):
    engine.nextseg = 2
    work = seed(engine, 1)
    snap = engine.snapshots['main']
    key = exact_key(work, 0)
    engine.c.execute('delete from rows where key<>?', (key,))
    engine.c.execute('alter table rows add column payload_digest blob')
    engine.c.execute('alter table rows add column bound_digest blob')
    for seg, term, value in list(engine.c.execute('select seg,key,value from rows')):
        engine.c.execute('update rows set payload_digest=?,bound_digest=? where seg=? and key=?', (hashlib.sha256(value).digest(), bound_digest(seg, term, value), seg, term))
    page = engine.c.execute("select rootpage from sqlite_master where name='rows'").fetchone()[0]
    print('checked posting before damage =', checked_postings(engine, snap, key))
    engine.c.wal_checkpoint(mode=b.apsw.SQLITE_CHECKPOINT_TRUNCATE)
    engine.c.execute('pragma journal_mode=delete')
    engine.c.close()
    with engine.path.open('r+b') as file:
        header = file.read(100)
        page_size = int.from_bytes(header[16:18], 'big')
        page_start = (page - 1) * page_size
        file.seek(page_start)
        data = file.read(page_size)
        assert data[0] == 10
        assert int.from_bytes(data[3:5], 'big') == 1
        cell = int.from_bytes(data[8:10], 'big')
        _, record = read_varint(data, cell)
        header_size, serial_at = read_varint(data, record)
        serial, _ = read_varint(data, serial_at)
        assert serial == 1
        offset = record + header_size
        assert data[offset] == 2
        file.seek(page_start + offset)
        file.write(bytes([data[offset] ^ 1]))
    engine.c = sqlite3.connect(engine.path.as_uri() + '?mode=ro', uri=True)
    print('damaged bytes = 1, flipped bits = 1')
    print('SQLite integrity_check =', engine.c.execute('pragma integrity_check').fetchone()[0])
    print('stored payload digest remains valid =', all(hashlib.sha256(value).digest() == digest for value, digest in engine.c.execute('select value,payload_digest from rows')))
    print('source document remains readable =', engine.source(0, snap, engine.c) is not None)
    engine.extra['point_row_checks'] = 0
    after = checked_postings(engine, snap, key)
    assert after == []
    print('checked posting after damage =', after)
    print('key-bound row digest checks reached =', engine.extra['point_row_checks'])
    print('ordinary S3+ posting after damage =', list(engine.query(snap, key, engine.c)))


def reclaim_payload(engine):
    work = seed(engine, 4)
    original = engine.snapshots['main']
    engine.root('worktree-being-removed', original['parts'], original['members'])
    seg = original['parts'][0][0]
    survivors = b.BitMap([2, 3])
    engine.root('main', [(seg, survivors)], survivors)
    reads = []

    def trace(cursor, statement, bindings):
        if statement.lower().startswith('select path,sha,n,payload from source'):
            reads.append(statement)
        return True

    engine.c.set_exec_trace(trace)
    engine.c.execute('begin immediate')
    engine.drop('worktree-being-removed')
    engine.reclaim()
    engine.c.execute('commit')
    engine.c.set_exec_trace(None)
    assert len(reads) == 2
    print('removed worktree is absent =', 'worktree-being-removed' not in engine.snapshots)
    print('unchanged source payload SELECTs during reclamation =', len(reads))
    print('payload query =', reads[0])
    print('compaction reason =', engine.merge_records[-1]['reason'])
    print('surviving exact postings =', [list(engine.query(engine.snapshots['main'], exact_key(work, doc), engine.c)) for doc in survivors])


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('probe', choices=['held-reader', 'corrupt-lookup', 'reclaim-payload'])
    args = parser.parse_args()
    SCRATCH.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=SCRATCH, prefix=args.probe + '-') as directory:
        engine = S3Plus(pathlib.Path(directory) / 'probe.sqlite')
        try:
            {'held-reader': held_reader, 'corrupt-lookup': corrupt_lookup, 'reclaim-payload': reclaim_payload}[args.probe](engine)
        finally:
            engine.close()


if __name__ == '__main__':
    main()
