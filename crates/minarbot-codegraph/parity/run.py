#!/usr/bin/env python3
"""Compare Minarbot CodeGraph with installed CodeGraph 1.6.2 on disposable clones.

The natural-key dump follows colbymchenry/codegraph v1.6.2 (6560052),
scripts/dump-graph.mjs, MIT (Copyright (c) 2026 Colby Mchenry).
"""
import argparse
from collections import Counter
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile

REPOS = [
    ("ripgrep", "BurntSushi/ripgrep", "14.1.1", "4649aa9700619f94cf9c66876e9549d83420e16c"),
    ("express", "expressjs/express", "4.21.2", "1faf228935aa0a13111f92c28ee795be64ce3f0f"),
    ("flask", "pallets/flask", "3.1.0", "ab8149664182b662453a563161aa89013c806dc9"),
    ("cobra", "spf13/cobra", "v1.9.1", "40b5bc1437a564fc795d388b23835e84f54cd1d1"),
    ("excalidraw", "excalidraw/excalidraw", "v0.18.0", "817d8c553c3389650f8b4503984a6d4a5d2f0c11"),
]
MVP = {"rust", "typescript", "tsx", "javascript", "jsx", "python", "go"}
LANGUAGE_REPOS = [
    ("junit4", "junit-team/junit4", "r4.13.2", "05fe2a64f59127c02135be22f416e91260d6ede6", "java"),
    ("cjson", "DaveGamble/cJSON", "v1.7.18", "acc76239bee01d8e9c858ae2cab296704e52d916", "c"),
    ("fmt", "fmtlib/fmt", "11.1.4", "123913715afeb8a437e6388b4473fcc4753e1c9a", "cpp"),
    ("dapper", "DapperLib/Dapper", "2.1.35", "360367ca5475425944fb390433cb0f1817ad2dcb", "csharp"),
    ("sinatra", "sinatra/sinatra", "v4.1.1", "7b50a1bbb5324838908dfaa00ec53ad322673a29", "ruby"),
    ("monolog", "Seldaek/monolog", "3.8.1", "aef6ee73a77a66e404dd6540934a9ef1b3c855b4", "php"),
    ("swift-argument-parser", "apple/swift-argument-parser", "1.5.0", "41982a3656a71c768319979febd796c6fd111d5c", "swift"),
    ("kotlinx-cli", "Kotlin/kotlinx-cli", "v0.3.6", "85737e0e4c97fccea5df1867bffdb545d2a9a75d", "kotlin"),
    ("glue", "tidyverse/glue", "v1.8.0", "48688f8f720a8f043b0f989b518a42bc32c410f0", "r"),
    ("luassert", "lunarmodules/luassert", "v1.9.0", "8d8dc8a54cc468048a128a867f6449a6c3fdd11a", "lua"),
    ("lune", "lune-org/lune", "v0.8.9", "010cd36375b467c85292ec87c922ee5806d79581", "luau"),
    ("scalaj-http", "scalaj/scalaj-http", "2.4.2", "afd73791ec53d38937a78e2b3ebce927e3ddcba1", "scala"),
    ("args", "dart-lang/args", "v2.5.0", "5c83bc9785d6c32ffe6824ba79fadcc51fbcd1c1", "dart"),
]
RESOLVED_KINDS = {"calls", "imports", "extends", "implements", "references", "instantiates"}
NATURAL = ("file_path", "kind", "qualified_name", "start_line", "start_column")
NODE_COLUMNS = (
    *NATURAL, "name", "language", "end_line", "end_column", "docstring", "signature",
    "visibility", "is_exported", "is_async", "is_static", "is_abstract",
    "decorators", "type_parameters", "return_type",
)
GIT_ENV = {**os.environ, "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1"}


def command(args, *, cwd, env=GIT_ENV, log=None):
    result = subprocess.run(
        args, cwd=cwd, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT, text=True, timeout=300, check=False,
    )
    if log:
        log.write_text(result.stdout, encoding="utf-8")
    if result.returncode != 0:
        raise RuntimeError(f"{args[0]} exited {result.returncode}\n{result.stdout}")
    return result.stdout.strip()


def prepare(scratch, name, repo, tag, commit):
    root = scratch / name
    command(
        ["git", "-c", "core.hooksPath=/dev/null", "clone", "--quiet", "--depth", "1",
         "--branch", tag, f"https://github.com/{repo}.git", root], cwd=scratch,
    )
    head = command(["git", "rev-parse", "HEAD"], cwd=root)
    if head != commit:
        raise RuntimeError(f"{name} expected {commit}, found {head}")
    return root


def upstream(root, cli, artifacts):
    index = root / ".codegraph"
    index.mkdir()
    # init is a one-shot index. On /tmp, watch policy is normal, so the
    # post-index hook-install fallback is skipped. Sandbox only permits
    # writes inside the disposable index and denies network access.
    sandbox = [
        "bwrap", "--die-with-parent", "--unshare-net", "--ro-bind", "/", "/",
        "--dev", "/dev", "--proc", "/proc", "--bind", index, index, "--chdir", root,
        "--setenv", "CODEGRAPH_DIR", ".codegraph",
        "--setenv", "CODEGRAPH_NO_DOWNLOAD", "1", "--setenv", "DO_NOT_TRACK", "1",
        "--setenv", "CODEGRAPH_NO_WATCH", "0", "--setenv", "CODEGRAPH_NO_DAEMON", "1",
        "--setenv", "CODEGRAPH_NO_WATCHDOG", "1", "--setenv", "CODEGRAPH_KERNEL", "1",
        "--unsetenv", "CODEGRAPH_KERNEL_LANGS",
    ]
    version = command([*sandbox, cli, "--version"], cwd=root)
    if version != "1.6.2":
        raise RuntimeError(f"expected CodeGraph 1.6.2, found {version!r}")
    command(
        [*sandbox, cli, "init", "--yes", root], cwd=root,
        log=artifacts / f"{root.name}-node.log" if artifacts else None,
    )
    return index / "codegraph.db"


def open_db(db):
    connection = sqlite3.connect(db.as_uri() + "?mode=ro", uri=True)
    connection.row_factory = sqlite3.Row
    return connection


def stable_json(source):
    return json.dumps(json.loads(source), sort_keys=True) if source is not None else None


def snapshot(connection, blocked, languages):
    files = {
        row["path"]: (row["language"], row["generated"]) for row in connection.execute(
            "SELECT path, language, generated FROM files"
        ) if row["language"] in languages and row["path"] not in blocked
    }
    nodes = {}
    downstream_nodes = set()
    for row in connection.execute("SELECT id, " + ", ".join(NODE_COLUMNS) + " FROM nodes"):
        if row["file_path"] not in files:
            continue
        if row["kind"] == "route":
            downstream_nodes.add(row["id"])
        else:
            nodes[row["id"]] = tuple(row[key] for key in NODE_COLUMNS)
    edges = []
    resolved = []
    unresolved = []
    downstream_edges = 0
    in_scope = nodes.keys() | downstream_nodes
    for row in connection.execute("SELECT source, target, kind, line, col, metadata, provenance FROM edges"):
        if row["source"] not in in_scope or row["target"] not in in_scope:
            continue
        if row["source"] in downstream_nodes or row["target"] in downstream_nodes:
            downstream_edges += 1
            continue
        source, target = nodes[row["source"]], nodes[row["target"]]
        metadata = json.loads(row["metadata"] or "null") or {}
        extraction_edge = (
            row["provenance"] is None and source[0] == target[0]
            and ((row["kind"] == "contains" and not metadata)
                 or (row["kind"] == "references" and metadata == {"valueRef": True}))
        )
        record = (
            source[:len(NATURAL)], target[:len(NATURAL)], row["kind"],
            row["line"], row["col"], stable_json(row["metadata"]), row["provenance"],
        )
        if extraction_edge:
            edges.append(record)
        elif row["kind"] in RESOLVED_KINDS:
            resolved.append(record)
        else:
            downstream_edges += 1
    for row in connection.execute(
        "SELECT from_node_id, reference_name, reference_kind, line, col, candidates, "
        "file_path, language, status, name_tail FROM unresolved_refs"
    ):
        if row["from_node_id"] in nodes and row["file_path"] in files:
            unresolved.append((nodes[row["from_node_id"]][:len(NATURAL)], *tuple(row)[1:5],
                               stable_json(row["candidates"]), *tuple(row)[6:]))
    return (
        {"files": Counter(files.items()), "nodes": Counter(nodes.values()),
         "extraction": Counter(edges), "resolved": Counter(resolved), "unresolved": Counter(unresolved)},
        len(downstream_nodes), downstream_edges,
    )


def errors(connection, languages):
    return {
        row["path"]: json.loads(row["errors"]) for row in connection.execute(
            "SELECT path, language, errors FROM files WHERE errors IS NOT NULL"
        ) if row["language"] in languages and json.loads(row["errors"])
    }


def dump(path, snapshot):
    with path.open("w", encoding="utf-8") as out:
        for kind, records in snapshot.items():
            for line in sorted(json.dumps((kind, record), ensure_ascii=True) for record in records.elements()):
                print(line, file=out)


def reference_key(row, metadata):
    if "refName" in metadata:
        return row[0], metadata["refName"], metadata.get("refKind", row[2]), row[3], row[4]
    return None


def edge_differences(left, right):
    """Account for each divergent edge and ref using its original call site."""
    edges = {row[:5] for row in right["resolved"]}
    resolved_sites = {reference_key(row, json.loads(row[5] or "{}")) for row in right["resolved"]}
    failed_sites = {row[:5] for row in right["unresolved"]}
    reasons = Counter()
    for row, count in (left["resolved"] - right["resolved"]).items():
        metadata = json.loads(row[5] or "{}")
        site = reference_key(row, metadata)
        if row[:5] in edges:
            reason = "edge metadata"
        elif "synthesizedBy" in metadata:
            reason = f"synthesis {metadata['synthesizedBy']}"
        elif site in resolved_sites and site is not None:
            reason = "other target or kind"
        elif site in failed_sites and site is not None:
            reason = "other side kept failed ref"
        else:
            reason = "edge without counterpart site"
        reasons[reason] += count
    return reasons


def compare(name, node_db, rust_db, artifacts, languages=MVP):
    with open_db(node_db) as node, open_db(rust_db) as rust:
        node_errors, rust_errors = errors(node, languages), errors(rust, languages)
        blocked = node_errors.keys() | rust_errors.keys()
        left, downstream_nodes, downstream_edges = snapshot(node, blocked, languages)
        right, _, _ = snapshot(rust, blocked, languages)
    if artifacts:
        dump(artifacts / f"{name}-node.jsonl", left)
        dump(artifacts / f"{name}-rust.jsonl", right)
        (artifacts / f"{name}-errors.json").write_text(
            json.dumps({"node": node_errors, "rust": rust_errors}, indent=2), encoding="utf-8",
        )
    counts = []
    differences = []
    details = []
    for kind in left:
        missing, extra = left[kind] - right[kind], right[kind] - left[kind]
        counts.append(f"{left[kind].total()}/{right[kind].total()}")
        differences.append(f"{missing.total()}/{extra.total()}")
        for label, records in [("Node only", missing), ("Rust only", extra)]:
            if records:
                details.append(f"  {label} {kind} sample {next(iter(records))!r}")
        if artifacts:
            with (artifacts / f"{name}-diff.jsonl").open("a" if kind != "files" else "w", encoding="utf-8") as out:
                for side, records in [("node", missing), ("rust", extra)]:
                    for line in sorted(json.dumps((side, kind, row), ensure_ascii=True) for row in records.elements()):
                        print(line, file=out)
    row = "| " + " | ".join([
        name, *counts, f"{len(node_errors)}/{len(rust_errors)}", *differences,
    ]) + " |"
    if left["resolved"] != right["resolved"]:
        for side, one, other in [("Node", left, right), ("Rust", right, left)]:
            reasons = edge_differences(one, other)
            if reasons:
                details.append(f"  {side} edge differences " + ", ".join(
                    f"{reason}={count}" for reason, count in sorted(reasons.items())
                ))
    return row, details, blocked, downstream_nodes, downstream_edges


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("index", type=Path, help="built example index executable")
    parser.add_argument("--node", type=Path, default=Path("/home/iqbal/.npm-global/bin/codegraph"))
    parser.add_argument("--artifacts", type=Path, help="save natural-key dumps, errors and CLI logs")
    repos = [(*repo, None) for repo in REPOS] + LANGUAGE_REPOS
    parser.add_argument("--repo", action="append", choices=[row[0] for row in repos],
                        help="compare only this pinned repository (repeatable)")
    args = parser.parse_args()
    binary, cli = args.index.resolve(strict=True), args.node.absolute()
    artifacts = args.artifacts.resolve() if args.artifacts else None
    if artifacts:
        artifacts.mkdir(parents=True, exist_ok=True)
    rows, notes = [], []
    with tempfile.TemporaryDirectory(prefix="minarbot-codegraph-parity-", dir="/tmp") as temp:
        scratch = Path(temp)
        for name, repo, tag, commit, language in repos:
            if args.repo and name not in args.repo:
                continue
            print(f"Indexing {repo} {commit}", file=sys.stderr, flush=True)
            root = prepare(scratch, name, repo, tag, commit)
            node_db = upstream(root, cli, artifacts)
            rust_db = scratch / f"{name}.db"
            command([binary, root, rust_db], cwd=root)
            row, details, blocked, downstream_nodes, downstream_edges = compare(name, node_db, rust_db, artifacts, {language} if language else MVP)
            rows.append(row)
            notes.append((name, details, sorted(blocked), downstream_nodes, downstream_edges))
    print("| Repository | Files | Nodes | Extraction edges | Resolved edges | Unresolved refs | Files with errors | File diff | Node diff | Extraction diff | Resolved diff | Unresolved diff |")
    print("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|")
    print("\n".join(rows))
    for name, details, blocked, downstream_nodes, downstream_edges in notes:
        print(f"\n{name}: {downstream_nodes} nodes, {downstream_edges} edges in Node downstream output")
        if blocked:
            print(f"  Files with errors: {', '.join(blocked)}")
        if details:
            print("\n".join(details))
    return int(any(details for _, details, _, _, _ in notes))


if __name__ == "__main__":
    sys.exit(main())
