"""Nanarchive benchmark: size and speed vs ZIP, 7-Zip and WinRAR.

Usage (from the repo root):
    python bench/bench.py                      # all categories, all tools
    python bench/bench.py -c code-js images    # some categories
    python bench/bench.py -t nax-max 7z-ultra  # some tools

Each category is a folder in bench/corpus/. Results are printed as a table and
saved to bench/results/<timestamp>.md and .json.
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CORPUS = ROOT / "bench" / "corpus"
RESULTS = ROOT / "bench" / "results"
NAX = ROOT / "target" / "release" / ("nax.exe" if os.name == "nt" else "nax")
SEVENZIP = Path(r"C:\Program Files\7-Zip\7z.exe")
RAR = Path(r"C:\Program Files\WinRAR\Rar.exe")


def nax(preset, extra=()):
    return {
        "ext": ".nax",
        "exe": NAX,
        "pack": lambda a, src: [NAX, "a", a, src, "-p", preset, "-y", *extra],
        "unpack": lambda a, out: [NAX, "x", a, "-o", out, "-y"],
    }


def sevenzip(args, ext=".7z"):
    return {
        "ext": ext,
        "exe": SEVENZIP,
        "pack": lambda a, src: [SEVENZIP, "a", *args, a, src, "-bso0", "-bsp0"],
        "unpack": lambda a, out: [SEVENZIP, "x", a, f"-o{out}", "-y", "-bso0", "-bsp0"],
    }


def rar(args):
    return {
        "ext": ".rar",
        "exe": RAR,
        "pack": lambda a, src: [RAR, "a", *args, "-r", "-idq", "-ep1", a, src],
        "unpack": lambda a, out: [RAR, "x", "-idq", "-o+", a, str(out) + os.sep],
    }


TOOLS = {
    "zip": sevenzip(["-tzip", "-mx5"], ".zip"),
    "7z-normal": sevenzip(["-t7z", "-mx5"]),
    "7z-ultra": sevenzip(["-t7z", "-mx9"]),
    "rar-normal": rar(["-m3", "-s", "-ma5"]),
    "rar-best": rar(["-m5", "-s", "-ma5", "-md64m"]),
    "nax-fast": nax("fast"),
    "nax-balanced": nax("balanced"),
    "nax-max": nax("max"),
    "nax-ultra": nax("ultra"),
}


def folder_size(p: Path):
    total = files = 0
    for dirpath, _, names in os.walk(p):
        for n in names:
            total += os.path.getsize(os.path.join(dirpath, n))
            files += 1
    return total, files


def warm_cache(p: Path):
    """Read everything once so no tool pays for cold disk reads."""
    for dirpath, _, names in os.walk(p):
        for n in names:
            with open(os.path.join(dirpath, n), "rb") as f:
                while f.read(1 << 20):
                    pass


def timed(cmd):
    t = time.perf_counter()
    r = subprocess.run([str(c) for c in cmd], capture_output=True)
    dt = time.perf_counter() - t
    if r.returncode != 0:
        msg = (r.stderr or r.stdout).decode(errors="replace").strip()
        raise RuntimeError(f"{Path(str(cmd[0])).name} failed: {msg[-400:]}")
    return dt


def run_one(tool, src: Path, work: Path):
    t = TOOLS[tool]
    archive = work / f"{src.name}-{tool}{t['ext']}"
    out = work / f"out-{tool}"
    if archive.exists():
        archive.unlink()
    pack_s = timed(t["pack"](archive, src))
    size = archive.stat().st_size
    unpack_s = timed(t["unpack"](archive, out))
    got, _ = folder_size(out)
    shutil.rmtree(out, ignore_errors=True)
    archive.unlink()
    return {"size": size, "pack_s": pack_s, "unpack_s": unpack_s, "unpacked": got}


def mb(n):
    return n / (1 << 20)


def main():
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    ap = argparse.ArgumentParser()
    ap.add_argument("-c", "--categories", nargs="*")
    ap.add_argument("-t", "--tools", nargs="*", default=list(TOOLS))
    ap.add_argument("--label", default="")
    args = ap.parse_args()

    if not NAX.exists():
        sys.exit(f"{NAX} not found: run `cargo build --release -p nax-cli` first")
    tools = [t for t in args.tools if TOOLS[t]["exe"].exists()]
    for t in set(args.tools) - set(tools):
        print(f"skipping {t}: {TOOLS[t]['exe']} not installed")
    cats = args.categories or sorted(p.name for p in CORPUS.iterdir() if p.is_dir())

    results = {}
    lines = []
    with tempfile.TemporaryDirectory(prefix="naxbench-") as tmp:
        work = Path(tmp)
        for cat in cats:
            src = CORPUS / cat
            total, files = folder_size(src)
            warm_cache(src)
            header = f"\n### {cat} — {files} files, {mb(total):.1f} MB\n"
            print(header)
            table = [
                "| Tool | Size MB | Ratio | Pack s | Pack MB/s | Unpack s |",
                "|---|---:|---:|---:|---:|---:|",
            ]
            print("\n".join(table), flush=True)
            results[cat] = {"bytes": total, "files": files, "tools": {}}
            for tool in tools:
                try:
                    r = run_one(tool, src, work)
                except RuntimeError as e:
                    print(f"| {tool} | error: {e} |")
                    continue
                if r["unpacked"] != total:
                    print(f"  ! {tool}: extracted {r['unpacked']} bytes, expected {total}")
                results[cat]["tools"][tool] = r
                row = (
                    f"| {tool} | {mb(r['size']):.2f} | {100 * r['size'] / total:.1f}% | "
                    f"{r['pack_s']:.1f} | {mb(total) / r['pack_s']:.1f} | {r['unpack_s']:.1f} |"
                )
                table.append(row)
                print(row, flush=True)
            lines.append(header + "\n".join(table))

    RESULTS.mkdir(parents=True, exist_ok=True)
    stamp = time.strftime("%Y%m%d-%H%M%S")
    name = stamp + (f"-{args.label}" if args.label else "")
    cpu = os.cpu_count()
    md = f"# Benchmark {stamp} {args.label}\n\nCPU threads: {cpu}\n" + "\n".join(lines) + "\n"
    (RESULTS / f"{name}.md").write_text(md, encoding="utf-8")
    (RESULTS / f"{name}.json").write_text(json.dumps(results, indent=2), encoding="utf-8")
    print(f"\nSaved bench/results/{name}.md")


if __name__ == "__main__":
    main()
