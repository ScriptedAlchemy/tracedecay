#!/usr/bin/env python3
"""Export the saved GitHub audit: python scripts/export-github-workload.py INPUT_DIR."""
import argparse
import hashlib
import json
from pathlib import Path


def read(path):
    return json.loads(path.read_text())


def identity(url):
    parts = url.removeprefix('https://github.com/').split('/')
    if len(parts) != 4 or parts[2] != 'pull' or not parts[3].isdigit():
        raise ValueError(f'Not a GitHub PR URL: {url}')
    return f'{parts[0]}/{parts[1]}#{parts[3]}'


def export(source):
    window = read(source / 'window.json')
    captured = window['fetched_at']
    nodes, bodies, details, queries = {}, {}, {}, []

    def add(row, cohort=None):
        key = identity(row['html_url'])
        # Cohort reads own their observed states; later detail reads supply head only.
        if key not in nodes:
            pull = row.get('pull_request', row)
            nodes[key] = dict(id=key, repo=key.split('#')[0], number=row['number'],
                              url=row['html_url'], title=row.get('title'),
                              state='merged' if pull.get('merged_at') else row.get('state'),
                              createdAt=row.get('created_at'), updatedAt=row.get('updated_at'),
                              closedAt=row.get('closed_at'), mergedAt=pull.get('merged_at'),
                              draft=row.get('draft'), cohorts=[], headSha=None,
                              headObservedAt=None, headSourceUrl=None)
        if cohort and cohort not in nodes[key]['cohorts']:
            nodes[key]['cohorts'].append(cohort)
        if row.get('body'):
            bodies[key] = row['body']
        return key

    for filename, cohort in [('created_last_7d', 'created'), ('merged_last_7d', 'merged'), ('open_backlog', 'open')]:
        result = read(source / f'{filename}.json')
        ids = {identity(row['html_url']) for row in result['items']}
        if result['incomplete_results'] or len(ids) != result['total_count'] or len(ids) > 1000:
            raise ValueError(f'Incomplete or duplicate cohort: {filename}')
        queries.append(dict(cohort=cohort, query=result['query'], total=len(ids),
                            pages=result['page_count'], incomplete=False))
        for row in result['items']:
            add(row, cohort)
    cohort_ids = set(nodes)
    # Discovery bodies are evidence only; do not inflate the workload with unrelated history.
    discovered = {}
    for path in sorted(source.glob('*-recent.json')):
        for row in read(path)['items']:
            key = identity(row['html_url'])
            discovered[key] = row
            bodies[key] = row.get('body') or ''
    for path in sorted((source / 'details').glob('*.json')):
        detail = read(path)
        key = identity(detail['pr']['html_url'])
        details[key] = detail
        discovered[key] = detail['pr']
        bodies[key] = detail['pr'].get('body') or ''

    def endpoint(key):
        if key not in nodes:
            if key in discovered:
                add(discovered[key])
            else:
                repo, number = key.split('#')
                add(dict(html_url=f'https://github.com/{repo}/pull/{number}', number=int(number)))
        return key

    relations = []

    def relation(origin, target, kind, evidence_owner, phrase):
        body = bodies[evidence_owner]
        if phrase not in body:
            raise ValueError(f'Relation evidence changed: {evidence_owner}: {phrase}')
        paragraph = next(part.strip() for part in body.split('\n\n') if phrase in part)
        edge_id = f'{origin}|{kind}|{target}'
        relations.append(dict(id=edge_id, **{'from': endpoint(origin)}, to=endpoint(target), type=kind,
                              evidence=dict(url=f'https://github.com/{evidence_owner.replace("#", "/pull/")}',
                                            excerpt=paragraph, observedAt=None,
                                            bodySha256=hashlib.sha256(body.encode()).hexdigest())))

    rspack = 'web-infra-dev/rspack#'
    mf = 'module-federation/core#'
    for previous, current in [(12977, 14911), (14911, 14912), (14912, 14913)]:
        relation(rspack + str(current), rspack + str(previous), 'prerequisite', rspack + str(current), f'Previous: #{previous}')
    for number in [12977, 14911, 14912, 14913]:
        relation(mf + '5039', rspack + str(number), 'companion', mf + '5039', 'Companion change')
    relation(rspack + '12978', mf + '5039', 'companion', rspack + '12978', 'companion change')
    relation('lynx-family/lynx-stack#3043', mf + '4907', 'consumer', 'lynx-family/lynx-stack#3043', 'downstream reference implementation')
    relation(mf + '4920', 'web-infra-dev/rstest#1407', 'prerequisite', mf + '4920', 'Builds on upstream')
    relation('web-infra-dev/rstest#1407', mf + '4320', 'companion', 'web-infra-dev/rstest#1407', 'This mode pairs with')
    owner = rspack + '14753'
    for target, kind, phrase in [('web-infra-dev/rsbuild#8091', 'consumer', 'Rsbuild consumer:'),
                                 ('rstackjs/rsbuild-plugin-react-router#97', 'withdrawn', 'Withdrawn plugin workaround:'),
                                 ('webpack/webpack#19494', 'precedent', 'Upstream precedent:'),
                                 (rspack + '14772', 'consolidation', 'Consolidated upstream version:')]:
        relation(owner, target, kind, owner, phrase)

    activity = []
    for key, detail in details.items():
        if 'reviews' not in detail:
            continue
        endpoint(key)
        pr = detail['pr']
        head = pr['head']['sha']
        events = []
        for field, kind in [('issue_comments', 'discussion'), ('reviews', 'review'), ('review_comments', 'inline review')]:
            for event in detail[field]:
                user = event.get('user') or {}
                if user.get('type') == 'Bot' or '[bot]' in user.get('login', ''):
                    continue
                if not (event.get('body') or '').strip() and event.get('state') not in ['APPROVED', 'CHANGES_REQUESTED']:
                    continue
                at = event.get('submitted_at') or event.get('updated_at') or event.get('created_at')
                if not at:
                    continue
                sha = event.get('commit_id')
                events.append(dict(at=at, url=event['html_url'], kind=kind, login=user['login'],
                                   commitSha=sha, matchesObservedHead=sha == head if sha else None))
        events.sort(key=lambda e: e['at'])
        author = [event for event in events if event['login'] == window['author']]
        external = [event for event in events if event['login'] != window['author']]
        last_author, last_external = author[-1] if author else None, external[-1] if external else None
        flags = []
        if last_author and last_external and last_author['at'] > last_external['at']:
            flags.append(dict(kind='author_activity_after_external', basis='Latest inspected meaningful author activity follows the latest external non-bot account response; follow-up candidate, not a blocker.'))
        if not external:
            flags.append(dict(kind='no_external_activity_in_sample', basis='No meaningful non-author non-bot account comments or reviews found in the fully paginated inspected surfaces; not evidence of no review elsewhere.'))
        activity.append(dict(prId=key, providerUpdatedAt=pr['updated_at'], observedHeadSha=head,
                             lastAuthorActivity=last_author, lastExternalActivity=last_external,
                             coverage=dict(issueComments=len(detail['issue_comments']), reviews=len(detail['reviews']), reviewComments=len(detail['review_comments'])), candidateFlags=flags))
    for key, node in nodes.items():
        if key in details:
            node['headSha'] = details[key]['pr']['head']['sha']
            node['headSourceUrl'] = f'https://api.github.com/repos/{node["repo"]}/pulls/{node["number"]}'
    prs = sorted(nodes.values(), key=lambda row: row['id'])
    ids = {row['id'] for row in prs}
    assert all(edge['from'] in ids and edge['to'] in ids for edge in relations)
    assert len({edge['id'] for edge in relations}) == len(relations)
    counts = {cohort: sum(cohort in row['cohorts'] for row in prs) for cohort in ['created', 'merged', 'open']}
    counts['weeklyUnique'] = sum(bool({'created', 'merged'} & set(row['cohorts'])) for row in prs)
    counts['openRepos'] = len({row['repo'] for row in prs if 'open' in row['cohorts']})
    counts['cohortUnique'] = len(cohort_ids)
    assert all(counts[query['cohort']] == query['total'] for query in queries)
    assert counts['weeklyUnique'] <= counts['created'] + counts['merged']
    return dict(schemaVersion=1, capturedAt=captured,
                window=dict(start=window['start_inclusive'], end=window['end_inclusive']),
                source=dict(author=window['author'], queries=queries,
                            limitations=['Saved authenticated GitHub snapshot; not live. Search access is limited to repositories visible to the token.',
                                         'Window membership is timestamp-bounded; PR states reflect fetch time. Detail reads followed search and may differ.',
                                         'capturedAt is the audit window anchor. Exact subsequent detail/body fetch times were not recorded; observedAt/headObservedAt are null.',
                                         'updatedAt is any provider activity, not human review. Non-bot accounts may be operated through automation.',
                                         'Review activity covers eight selected old-open PRs only. No check-run results or repository merge policies were exported.',
                                         'matchesObservedHead compares captured commit IDs only; it does not establish current approval or merge readiness.',
                                         'Cross-repository relations are a curated explicit-body sample, not an exhaustive graph. URL-only endpoints have null state/title/timestamps.',
                                         'Link discovery: Rspack50/50, Rstest3/3, Lynx1/1, Module Federation core latest-updated100/862. No timing/topic inferred links.']),
                counts=counts, prs=prs, relations=relations, reviewActivity=activity,
                relationDirections=dict(prerequisite='from depends on the explicit previous stack slice or upstream support to',
                                        companion='from explicitly pairs with to; no merge ordering implied',
                                        consumer='from is upstream of the named consumer to',
                                        precedent='from names to as an upstream precedent',
                                        withdrawn='from names to as a withdrawn workaround',
                                        consolidation='from points to the consolidated upstream version to'))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('input_dir', type=Path)
    parser.add_argument('--output', type=Path, default=Path(__file__).resolve().parents[1] / 'src/data/github-workload.json')
    args = parser.parse_args()
    result = export(args.input_dir)
    args.output.write_text(json.dumps(result, separators=(',', ':'), ensure_ascii=False) + '\n')
    print(json.dumps({'output': str(args.output), 'counts': result['counts'], 'relations': len(result['relations']), 'reviewSample': len(result['reviewActivity'])}))


if __name__ == '__main__':
    main()
