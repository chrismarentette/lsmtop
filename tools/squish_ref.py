import math
import json
import os

def lcg(seed, count):
    val = seed
    out = []
    for _ in range(count):
        val = (val * 1103515245 + 12345) & 0x7FFFFFFF
        out.append(float(val) / 0x7FFFFFFF * 100.0)
    return out

def python_squish(vals, w):
    n = len(vals)
    spans = []
    for i in range(w):
        x = (i + 0.5) / float(w)
        spans.append(1.0 if x >= 2.0 / 3.0 else
                     (2.0 if x >= 1.0 / 3.0 else 3.0))
    mult = float(n) / sum(spans)
    edges = [0.0] * (w + 1)
    edges[w] = -0.5
    for i in range(w - 1, -1, -1):
        edges[i] = edges[i + 1] + spans[i] * mult
    out = []
    for i in range(w):
        hi = edges[i]
        lo = edges[i + 1]
        kmin = int(math.floor(lo + 1e-9)) + 1
        kmax = min(n - 1, int(math.floor(hi + 1e-9)))
        if kmin <= kmax:
            out.append(max(vals[n - 1 - kmax: n - kmin]))
        else:
            mid = (lo + hi) / 2.0
            fi = min(max(n - 1 - mid, 0.0), float(n - 1))
            i0_ = int(fi)
            i1_ = min(i0_ + 1, n - 1)
            fr = fi - i0_
            out.append(vals[i0_] * (1.0 - fr) + vals[i1_] * fr)
    return out

if __name__ == "__main__":
    raw = lcg(42, 200)
    frames = {}
    for tick in range(31):
        # We just squish the same raw array, as the translated 
        # python function only depends on raw and w.
        frames[str(tick)] = python_squish(raw, 60)
        
    os.makedirs("tests/fixtures", exist_ok=True)
    with open("tests/fixtures/squish_golden.json", "w") as f:
        json.dump(frames, f, indent=2)
    print("generated tests/fixtures/squish_golden.json")
