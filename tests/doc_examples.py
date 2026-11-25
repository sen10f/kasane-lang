#!/usr/bin/env python3
"""Extract complete ksnl code blocks from the docs and run `ksn check` on each.

Usage: python3 tests/doc_examples.py [--ksn /path/to/ksn] [file ...]

Scans the given markdown files (default: BETA.md, README.md, チートシート.md,
docs/*.md). Every code fence whose info string is empty, `kasane` or `ksnl` and is
not `ksnl fragment` is treated as a complete ksnl example: it is copied into a
scratch package and `ksn check` (and `ksn fmt --check`) is run. Failures are
reported as `file:line`.

Fragments by design must be fenced with `ksnl fragment` and are skipped. Other
fences (`bash`, `text`, `c`, `toml` ...) are skipped.
"""

import argparse
import glob
import os
import re
import subprocess
import sys
import tempfile

DEFAULT_KSN = "/home/kasane/kasane-verify/bin/current/ksn"

CHECKABLE_INFO = {"", "kasane", "ksnl"}

TOP_LEVEL_DEF = re.compile(r"^(fn|type|enum|const|extern|test|impl|trait)\b")


def iter_fences(path):
    with open(path, encoding="utf-8") as fh:
        lines = fh.read().split("\n")
    i = 0
    while i < len(lines):
        line = lines[i]
        m = re.match(r"^```([^\n]*)", line)
        if not m:
            i += 1
            continue
        info = m.group(1)
        start = i
        i += 1
        code = []
        while i < len(lines) and not lines[i].startswith("```"):
            code.append(lines[i])
            i += 1
        end = i  # line of closing fence
        yield info, "\n".join(code), start + 1, end + 1
        i += 1


def has_top_level_def(code):
    return any(TOP_LEVEL_DEF.match(line) for line in code.split("\n"))


def module_of(code):
    m = re.search(r"^\s*(?:fn|type|enum|const|extern|test|impl|trait)\s+([A-Za-z_][\w.]*)", code, re.M)
    if m:
        return m.group(1).split(".")[0]
    return "doc"


def run(cmd, env, cwd=None):
    return subprocess.run(cmd, env=env, capture_output=True, text=True, cwd=cwd)


def check_block(info, code, file, line, ksn, cache):
    info = info.strip()
    if info == "ksnl fragment":
        return None  # fragment by design: skip
    if info not in CHECKABLE_INFO:
        return None  # bash / text / c / toml ...: not ksnl
    if not has_top_level_def(code):
        return (
            file,
            line,
            "not a complete definition (no fn/type/enum/const/extern/test/impl/trait); "
            "fence with `ksnl fragment` if it is a fragment by design",
        )

    mod = module_of(code)
    entry = mod + ".main"

    tmp = tempfile.mkdtemp(prefix="doc_ex_")
    try:
        with open(os.path.join(tmp, "kasane.toml"), "w", encoding="utf-8") as fh:
            fh.write("[package]\n")
            fh.write('name = "doc_ex"\n')
            fh.write('version = "0.1.0"\n')
            fh.write('edition = 1\n')
            fh.write('entry = "%s"\n' % entry)
            fh.write("\n[dependencies]\n")
        with open(os.path.join(tmp, "kasane.lock"), "w", encoding="utf-8") as fh:
            fh.write("version = 1\n")
        os.makedirs(os.path.join(tmp, "src"))
        with open(os.path.join(tmp, "src", mod + ".ksnl"), "w", encoding="utf-8") as fh:
            fh.write(code)
            if not code.endswith("\n"):
                fh.write("\n")

        env = dict(os.environ)
        env["KSN_CACHE_DIR"] = cache

        res = run([ksn, "check"], env, cwd=tmp)
        if res.returncode != 0:
            return (file, line, res.stderr.strip() or res.stdout.strip())

        res = run([ksn, "fmt", "--check"], env, cwd=tmp)
        if res.returncode != 0:
            return (file, line, "fmt: " + (res.stderr.strip() or res.stdout.strip()))
        return None
    finally:
        subprocess.run(["rm", "-rf", tmp], capture_output=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--ksn", default=os.environ.get("KSN", DEFAULT_KSN))
    ap.add_argument("--cache", default=os.environ.get("KSN_CACHE_DIR", "/tmp/ksn_cache"))
    ap.add_argument("files", nargs="*")
    args = ap.parse_args()

    if not os.path.exists(args.ksn):
        print("ksn not found: %s" % args.ksn, file=sys.stderr)
        return 2

    files = args.files
    if not files:
        files = ["BETA.md", "README.md", "チートシート.md"] + sorted(
            glob.glob("docs/*.md")
        )

    failures = 0
    for file in files:
        if not os.path.exists(file):
            print("missing: %s" % file, file=sys.stderr)
            failures += 1
            continue
        for info, code, start, end in iter_fences(file):
            problem = check_block(info, code, file, start, args.ksn, args.cache)
            if problem is None:
                continue
            failures += 1
            f, line, msg = problem
            print("%s:%s: %s" % (f, line, msg))

    print("doc_examples: %d failure(s)" % failures, file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
