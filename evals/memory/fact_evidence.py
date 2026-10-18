"""Read the current retained CLI fact contract without inventing missing data."""


class ExactFactStoreError(RuntimeError):
    pass


def retained_payload(response, operation):
    outcome = response.get("outcome") if isinstance(response, dict) else None
    if not isinstance(outcome, dict) or outcome.get("outcome") not in ("evidence", "effect"):
        raise ExactFactStoreError(f"{operation} did not return retained evidence or an effect")
    value = outcome.get("value")
    payload = value.get("payload") if isinstance(value, dict) else None
    if not isinstance(payload, dict):
        raise ExactFactStoreError(f"{operation} omitted its retained payload")
    return payload


def require_fact(record, operation):
    if not isinstance(record, dict) or not isinstance(record.get("fact_id"), str) or not record["fact_id"]:
        raise ExactFactStoreError(f"{operation} omitted a string FactId")
    if not isinstance(record.get("content"), str):
        raise ExactFactStoreError(f"{operation} omitted available fact content")
    if "source_label" not in record or record["source_label"] is not None and not isinstance(record["source_label"], str):
        raise ExactFactStoreError(f"{operation} omitted its typed source label")
    trust = record.get("trust_score_millionths")
    if type(trust) is not int or not 0 <= trust <= 1_000_000:
        raise ExactFactStoreError(f"{operation} omitted its canonical trust score")
    telemetry = record.get("telemetry")
    retrievals = telemetry.get("retrieval_count") if isinstance(telemetry, dict) else None
    if type(retrievals) is not int or retrievals < 0:
        raise ExactFactStoreError(f"{operation} omitted retrieval telemetry")
    return record


def available_fact(projection, operation):
    if not isinstance(projection, dict) or projection.get("kind") != "available":
        raise ExactFactStoreError(f"{operation} did not expose an available fact projection")
    return require_fact(projection.get("fact"), operation)


def added_fact(response, operation):
    if not isinstance(response, dict) or response.get("outcome") != "committed":
        raise ExactFactStoreError(f"{operation} did not commit the seeded fact")
    result = response.get("result")
    if not isinstance(result, dict) or result.get("disposition") not in (
        "added", "near_duplicate", "possible_conflict"
    ):
        raise ExactFactStoreError(f"{operation} returned an unrecognized add disposition")
    fact = available_fact(result.get("fact"), operation)
    receipt = result.get("commit")
    if not isinstance(receipt, dict) or receipt.get("fact_id") != fact["fact_id"]:
        raise ExactFactStoreError(f"{operation} returned no matching fact commit receipt")
    return fact


def listed_facts(response, operation):
    values = response.get("facts") if isinstance(response, dict) else None
    if not isinstance(values, list):
        raise ExactFactStoreError(f"{operation} omitted its fact projections")
    return [available_fact(value, operation) for value in values]


def search_hits(response, operation):
    hits = response.get("hits") if isinstance(response, dict) else None
    if not isinstance(hits, list):
        raise ExactFactStoreError(f"{operation} omitted its search hits")
    return [require_fact(hit.get("fact") if isinstance(hit, dict) else None, operation) for hit in hits]


def next_after(response, seen):
    if "next_after" not in response:
        raise ExactFactStoreError("fact listing omitted its continuation state")
    cursor = response["next_after"]
    if cursor is None:
        return None
    if not isinstance(cursor, str) or not cursor or cursor in seen:
        raise ExactFactStoreError("fact listing returned an invalid or repeated continuation")
    seen.add(cursor)
    return cursor
