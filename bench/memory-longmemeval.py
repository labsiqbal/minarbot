#!/usr/bin/env python3
"""Reduced LongMemEval-S retrieval comparison. Usage documented in bench/README.md."""

import argparse
import datetime
import hashlib
import json
import os
import subprocess
import uuid
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import Request, urlopen

DATASET_REVISION = "98d7416c24c778c2fee6e6f3006e7a073259d48f"
HTTP_ROOT = "http://127.0.0.1:8888"
QUESTION_IDS = (
    "e47becba", "7161e7e2", "8a2466db", "6a1eabeb", "0a995998", "gpt4_59149c77",
)


def cases(dataset):
    selected = {item["question_id"]: item for item in dataset if item["question_id"] in QUESTION_IDS}
    for question_id in QUESTION_IDS:
        item = selected[question_id]
        gold = set(item["answer_session_ids"])
        sessions = list(zip(item["haystack_session_ids"], item["haystack_dates"], item["haystack_sessions"], strict=True))
        distractors = sorted(
            {session for session, _, _ in sessions} - gold,
            key=lambda session: hashlib.sha256(f"{question_id}/{session}".encode()).digest(),
        )[:7]
        documents = []
        for session, date, content in sessions:
            if session not in gold and session not in distractors:
                continue
            # Use the same date and message chunks for all three stores.
            time = datetime.datetime.strptime(date, "%Y/%m/%d (%a) %H:%M").replace(tzinfo=datetime.timezone.utc)
            for message in content:
                raw = message["content"]
                for offset in range(0, len(raw), 1700):
                    text = f"{date} {message['role']}: {raw[offset:offset+1700]}"
                    documents.append({"session": session, "text": text, "timestamp": time.isoformat()})
        yield {"id": question_id, "type": item["question_type"], "question": item["question"],
               "gold": sorted(gold), "documents": documents,
               "sessions": len(gold) + len(distractors), "full_sessions": len(sessions)}


def request(path, method="GET", data=None):
    payload = None if data is None else json.dumps(data).encode()
    headers = {"Content-Type": "application/json"}
    if token := os.environ.get("MINARBOT_HINDSIGHT_BENCH_TOKEN"):
        headers["Authorization"] = f"Bearer {token}"
    req = Request(HTTP_ROOT + path, payload, headers, method=method)
    with urlopen(req, timeout=150) as response:
        return json.load(response) if response.length != 0 else None


def hindsight(case):
    bank = f"minarbot-48-{uuid.uuid4().hex}"
    path = f"/v1/default/banks/{bank}"
    try:
        request(path, "PUT", {"retain_extraction_mode": "verbatim", "enable_observations": False})
        result = request(path + "/memories", "POST", {
            "items": [{"content": doc["text"], "timestamp": doc["timestamp"], "context": doc["session"],
                       "document_id": f"chunk-{index}", "metadata": {"session": doc["session"]}}
                      for index, doc in enumerate(case["documents"])],
            "async": False,
        })
        if result.get("success") is False:
            raise RuntimeError("Hindsight retain returned failure")
        result = request(path + "/memories/recall", "POST", {
            "query": case["question"], "budget": "mid", "max_tokens": 4096,
        })
        sessions = [hit.get("context") or (hit.get("metadata") or {}).get("session")
                    for hit in result["results"][:5]]
        return sessions
    finally:
        request(path, "DELETE")


def measure(gold, sessions):
    overlap = len(set(gold).intersection(sessions))
    rank = next((index + 1 for index, session in enumerate(sessions) if session in gold), None)
    return {"found": overlap, "total": len(gold), "first_rank": rank}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("dataset", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--minarbot-example", type=Path)
    parser.add_argument("--models", type=Path)
    parser.add_argument("--hindsight-results", type=Path)
    args = parser.parse_args()

    sample = list(cases(json.loads(args.dataset.read_text())))
    if args.hindsight_results:
        external = json.loads(args.hindsight_results.read_text())
    else:
        version = request("/openapi.json")["info"]["version"]
        external = {"version": version, "cases": {}}
        for case in sample:
            try:
                external["cases"][case["id"]] = hindsight(case)
            except HTTPError as error:
                body = error.read(1024)
                detail = " (PostgreSQL shared memory exhausted)" if b"could not resize shared memory segment" in body else ""
                external["blocked"] = f"HTTP {error.code}{detail}"
                print(f"Hindsight blocked: {external['blocked']}", flush=True)
                break
            print(f"{case['id']}: Hindsight retrieved; throwaway bank removed", flush=True)
    if not args.minarbot_example:
        args.output.write_text(json.dumps(external, indent=2) + "\n")
        return
    if not args.models:
        parser.error("--models is required with --minarbot-example")
    local = json.loads(subprocess.check_output(
        [args.minarbot_example, args.models], input=json.dumps(sample).encode()
    ))
    by_id = {result["id"]: result for result in local}
    rows = []
    for case in sample:
        result = by_id[case["id"]]
        ranked = {key: result[key] for key in ("legacy", "keyword", "hybrid")}
        if case["id"] in external["cases"]:
            ranked["hindsight"] = external["cases"][case["id"]]
        rows.append({
            **{key: case[key] for key in ("id", "type", "gold", "sessions", "full_sessions")},
            "documents": len(case["documents"]),
            "ranked_sessions": ranked,
            "metrics": {key: measure(case["gold"], sessions) for key, sessions in ranked.items()},
            "keyword_ms": round(result["keyword_ms"], 2),
            "hybrid_ms": round(result["hybrid_ms"], 2),
        })
    args.output.write_text(json.dumps({
        "dataset_revision": DATASET_REVISION, "hindsight_version": external["version"],
        "hindsight_blocked": external.get("blocked"),
        "measure": "source session recall and first relevant rank at five retrieved chunks",
        "cases": rows,
    }, indent=2) + "\n")


if __name__ == "__main__":
    main()
