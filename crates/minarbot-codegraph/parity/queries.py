#!/usr/bin/env python3
"""Compare MCP retrieval on pinned Elasticsearch and Minarbot snapshots.

The Elasticsearch suite reuses the queries and expected symbols from
colbymchenry/codegraph v1.6.2 (6560052), __tests__/evaluation/test-cases.ts,
MIT (Copyright (c) 2026 Colby Mchenry).
"""
import argparse
from contextlib import contextmanager
import json
import os
from pathlib import Path
import queue
import re
import shutil
import subprocess
import threading
from statistics import mean

from run import command, upstream

PIN = "6560052a6f856855d3f71eee838fd66ccfa4285d"
FILES = re.compile(r"^(?:### (.+)|\*\*`([^`]+)`\*\*)", re.MULTILINE)
MINARBOT = [
    ("ProjectHome Projects.tsx", ["ProjectHome"]),
    ("useProjects ProjectSidebar", ["useProjects", "ProjectSidebar"]),
    ("CreateProject FolderBrowser", ["CreateProject", "FolderBrowser"]),
    ("ProjectSourceControl SourceControl", ["ProjectSourceControl", "SourceControl"]),
    ("createQueryCache queryCache", ["createQueryCache", "queryCache"]),
    ("Switch Row Badge app/src/ui/kit.tsx", ["Switch", "Row", "Badge"]),
    ("ResultTarget resultTarget", ["ResultTarget", "resultTarget"]),
    ("CodeGraph Bridge codegraph.rs", ["Bridge"]),
    ("project_context workspace crates/minarbot-server/src/turns/tools.rs", ["project_context", "workspace"]),
    ("ProbeHome configure_mise mise_shim", ["ProbeHome", "configure_mise", "mise_shim"]),
    ("migrate_user_version migrate_canonical", ["migrate_user_version", "migrate_canonical"]),
    ("candidate_state enrollment_descriptor", ["candidate_state", "enrollment_descriptor"]),
    ("routine_schedule workflow_schedule", ["routine_schedule", "workflow_schedule"]),
    ("generated.rs detect crates/minarbot-codegraph/src/generated.rs", ["detect"]),
    ("Index sync crates/minarbot-codegraph/src/lib.rs", ["Index", "sync"]),
    ("How does project folder validation work?", ["validate", "open_directory"]),
    ("How are harness models discovered?", ["availability", "model_catalog"]),
    ("Which code handles project routines overdue occurrences?", ["overdue", "routine_schedule"]),
    ("How does the resolver retry unresolved references?", ["resolve", "retry"]),
    ("Where does a paused turn resume work?", ["resume", "promote"]),
    ("How does Minarbot revoke MCP session tokens?", ["revoke_session", "mint"]),
    ("How is project history displayed?", ["ProjectHome", "history"]),
    ("How does browser page evaluation reach Chrome?", ["evaluate", "request"]),
    ("How does the app cache and refresh queries?", ["createQueryCache", "useQueryPolling"]),
]


def upstream_cases(root):
    if command(["git", "rev-parse", "HEAD"], cwd=root) != PIN:
        raise RuntimeError("Expected CodeGraph 1.6.2 sources at " + PIN)
    source = (root / "__tests__/evaluation/test-cases.ts").read_text()
    cases = re.findall(
        r"id: '([^']+)',\s+query: '([^']+)',\s+api: '([^']+)',\s+expectedSymbols: (\[[^\]]+\])"
        r",\s*(?:kinds: \['([^']+)'\])?", source,
    )
    if len(cases) != 12:
        raise RuntimeError(f"Expected 12 evaluation cases, found {len(cases)}")
    for ident, query, api, expected, kind in cases:
        args = {"query": query}
        tool = "codegraph_explore"
        if api == "searchNodes":
            tool = "codegraph_search"
            if kind:
                args["kind"] = kind
        yield ident, tool, args, re.findall(r"'([^']+)'", expected)


@contextmanager
def session(argv, root, env, log):
    with log.open("w") as stderr, subprocess.Popen(
        argv, cwd=root, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=stderr, encoding="utf-8",
    ) as process:
        responses = queue.Queue()
        reader = threading.Thread(
            target=lambda: [responses.put(line) for line in process.stdout], daemon=True,
        )
        reader.start()
        sequence = 0

        def rpc(method, params):
            nonlocal sequence
            sequence += 1
            process.stdin.write(json.dumps({
                "jsonrpc": "2.0", "id": sequence, "method": method, "params": params,
            }) + "\n")
            process.stdin.flush()
            while True:
                response = json.loads(responses.get(timeout=90))
                if response.get("id") == sequence:
                    if "error" in response:
                        raise RuntimeError(response["error"])
                    return response["result"]

        try:
            rpc("initialize", {"protocolVersion": "2024-11-05", "capabilities": {},
                               "clientInfo": {"name": "query-parity", "version": "1"}})
            process.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
            process.stdin.flush()

            def call(name, args):
                result = rpc("tools/call", {"name": name, "arguments": args})
                if result.get("isError"):
                    raise RuntimeError(result)
                return "\n".join(value["text"] for value in result["content"])

            yield call
        finally:
            process.terminate()
            process.wait(timeout=15)
            reader.join(timeout=2)


def summary(text, expected, tool):
    # Exclude the echoed query, call-path text, and trailing follow-up suggestions
    # from symbol recall. A definition can appear as a signature or numbered line.
    start = FILES.search(text)
    if tool == "codegraph_explore":
        body = text[start.start():] if start else ""
        body = body.split("**Not shown above", 1)[0]
        paths = {a or b for a, b in FILES.findall(body)}
    else:
        body = text.partition("\n")[2]
        paths = set(re.findall(r" - ([^\n]+?):[0-9]+", body))
        paths.update(re.findall(r"(?m)^([^\n`]+):[0-9]+$", body))
    found = [name for name in expected if re.search(r"(?<!\w)" + re.escape(name) + r"(?!\w)", body, re.I)]
    return {"recall": len(found) / len(expected), "found": found, "files": sorted(paths)}


def overlap(a, b):
    a, b = set(a["files"]), set(b["files"])
    return len(a & b) / len(a | b) if a or b else 1.0


def compare(opts, root, cases, build_native):
    node_db = root / ".codegraph/codegraph.db"
    if not node_db.is_file():
        node_db = upstream(root, opts.cli, opts.artifacts)
    native_db = opts.artifacts / f"{root.name}-native.db"
    if build_native:
        print(command([opts.binary, "codegraph", "sync", root, native_db], cwd=root))
    env = {**os.environ, "CODEGRAPH_NO_DAEMON": "1", "CODEGRAPH_NO_WATCH": "1",
           "CODEGRAPH_NO_WATCHDOG": "1", "DO_NOT_TRACK": "1", "CODEGRAPH_NO_UPDATE_CHECK": "1",
           "CODEGRAPH_DIR": ".codegraph", "CODEGRAPH_MCP_TOOLS": "search,explore"}
    node = ["bwrap", "--die-with-parent", "--unshare-net", "--ro-bind", "/", "/",
            "--dev", "/dev", "--proc", "/proc", "--tmpfs", "/tmp",
            "--bind", root, root, "--chdir", root, opts.cli,
            "serve", "--mcp", "--no-watch", "--path", root]
    # When checkouts also live under /tmp, the nested writable mount is
    # recreated from the host path by bwrap after its private tmpfs.
    rust = [opts.binary, "codegraph", "stdio", root]
    results = {}
    with session(node, root, env, opts.artifacts / f"{root.name}-serve-node.log") as node_call:
        variants = {
            "Node/Node": [node_call(verb,args) for _, verb, args, _ in cases],
        }
    for label, database in [("Rust/Node", node_db), ("Rust/Rust", native_db)]:
        if label == "Rust/Rust" and not build_native:
            continue
        with session([*rust, database], root, env, opts.artifacts / f"{root.name}-{database.name}-rust.log") as call:
            variants[label] = [call(verb,args) for _, verb, args, _ in cases]
    for i, (ident, verb, _args, expected) in enumerate(cases):
        row = {label: summary(texts[i], expected, verb) for label, texts in variants.items()}
        baseline = row["Node/Node"]
        for stats in row.values():
            stats["overlap"] = overlap(baseline, stats)
        results[ident] = row
        scores = ", ".join(f"{label} {stats['recall']:.2f} recall/{stats['overlap']:.2f} files" for label, stats in row.items())
        print(f"{ident}: {scores}", flush=True)
    for label in variants:
        recall = mean(row[label]["recall"] for row in results.values())
        files = mean(row[label]["overlap"] for row in results.values())
        print(f"{root.name:15s} {label:10s} {len(results)} queries: {recall:.3f} recall, {files:.3f} file overlap")
    (opts.artifacts / f"{root.name}-query-parity.json").write_text(json.dumps(results, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=lambda s: Path(s).resolve())
    parser.add_argument("--upstream-sources", type=lambda s: Path(s).resolve(), required=True)
    parser.add_argument("--minarbot", type=lambda s: Path(s).resolve(), required=True)
    parser.add_argument("--elasticsearch", type=lambda s: Path(s).resolve(), required=True)
    parser.add_argument("--artifacts", type=lambda s: Path(s).resolve(), required=True)
    parser.add_argument("--cli", default=shutil.which("codegraph"))
    opts = parser.parse_args()
    if command([opts.cli, "--version"], cwd=opts.artifacts.parent) != "1.6.2":
        raise RuntimeError("Expected installed CodeGraph 1.6.2")
    opts.artifacts.mkdir(parents=True, exist_ok=True)
    cases = list(upstream_cases(opts.upstream_sources))
    compare(opts, opts.elasticsearch, cases, build_native=False)
    cases = [(query, "codegraph_explore", {"query": query}, names) for query, names in MINARBOT]
    compare(opts, opts.minarbot, cases, build_native=True)


if __name__ == "__main__":
    main()
