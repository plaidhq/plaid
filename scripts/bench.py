#!/usr/bin/env python3
# Replays TTS clips through `Plaid --file` (real-time pace) and reports latency + CER.
# Usage: PLAID_API_KEY=... scripts/bench.py [runs]
import os, re, subprocess, sys, statistics, unicodedata

root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
plaid = os.path.join(root, "build/Plaid.app/Contents/MacOS/Plaid")
clips = os.path.join(root, ".build/clips")
os.makedirs(clips, exist_ok=True)
runs = int(sys.argv[1]) if len(sys.argv) > 1 else 1

def norm(s):  # drop punctuation/space, lowercase; CER is per character
    return "".join(c for c in s.lower() if not unicodedata.category(c).startswith(("P", "Z")))

def dist(a, b):
    prev = list(range(len(b) + 1))
    for i, x in enumerate(a, 1):
        cur = [i]
        for j, y in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (x != y)))
        prev = cur
    return prev[-1]

rows = []
for n, line in enumerate(open(os.path.join(root, "scripts/cases.tsv")), 1):
    voice, ref = line.rstrip("\n").split("\t")
    wav = f"{clips}/{n}.wav"
    if not os.path.exists(wav):
        subprocess.run(["say", "-v", voice, "-o", wav, "--file-format=WAVE", "--data-format=LEI16@16000", ref], check=True)
    secs = (os.path.getsize(wav) - 44) / 32000
    for _ in range(runs):
        out = subprocess.run([plaid, "--file", wav], capture_output=True, text=True, timeout=120).stdout
        first = re.search(r"^\s*([\d.]+)s ", out, re.M)
        fin = re.search(r"^final \((\d+) ms after release\): (.*)$", out, re.M)
        if not fin:
            print(f"#{n} FAILED: {out.strip()[-200:]}")
            continue
        hyp = fin.group(2)
        cer = dist(norm(ref), norm(hyp)) / len(norm(ref))
        rows.append((float(first.group(1)) * 1000, int(fin.group(1)), cer))
        print(f"#{n} {secs:4.1f}s audio | first text {rows[-1][0]:4.0f} ms | release→final {rows[-1][1]:4d} ms | CER {cer:5.1%}\n   ref: {ref}\n   hyp: {hyp}")

if rows:
    for i, name in [(0, "first text ms"), (1, "release→final ms")]:
        v = sorted(r[i] for r in rows)
        print(f"{name}: median {statistics.median(v):.0f}, p90 {v[int(len(v) * 0.9) - (len(v) >= 10)]:.0f}, max {v[-1]:.0f}")
    print(f"mean CER {statistics.mean(r[2] for r in rows):.2%} over {len(rows)} runs")
