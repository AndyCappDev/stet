#!/usr/bin/env python3
"""clip_aa_sweep.py <file>: pages 1-3 of a sample; for each page the new build
changes, measure it against the supersampled reference. One JSON line each."""
import glob, importlib.util, json, os, resource, shutil, subprocess, sys, tempfile
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
spec = importlib.util.spec_from_file_location("m", ROOT + "/scripts/clip_aa_measure.py")
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
BASE, NEW = os.environ.get("BASE", ROOT + "/target/base/release/stet"), os.environ.get("NEW", ROOT + "/target/release/stet")
SCR = os.environ["SCR"]
def lim(): resource.setrlimit(resource.RLIMIT_AS, (24 << 30, 24 << 30))
def run(binary, src, out_dir, dpi, pages, extra=()):
    os.makedirs(out_dir, exist_ok=True)
    name = os.path.basename(src)
    shutil.copy(src, os.path.join(out_dir, name))
    try:
        subprocess.run([binary, "--device", "png", "--dpi", str(dpi), "--pages", pages, *extra, name],
                       cwd=out_dir, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=600, preexec_fn=lim)
    except subprocess.TimeoutExpired:
        pass
    os.unlink(os.path.join(out_dir, name))
    return sorted(glob.glob(os.path.join(out_dir, "*.png")))
f = sys.argv[1]
with tempfile.TemporaryDirectory(dir=SCR) as tmp:
    a = run(BASE, f, tmp + "/a", 150, "1-3")
    b = run(NEW, f, tmp + "/b", 150, "1-3")
    if len(a) != len(b):
        print(json.dumps({"file": f, "error": "page count %d vs %d" % (len(a), len(b))})); sys.exit()
    for i, (pa, pb) in enumerate(zip(a, b)):
        if open(pa, "rb").read() == open(pb, "rb").read():
            continue
        r = run(BASE, f, tmp + "/r%d" % i, 1200, str(i + 1), ["--no-aa"])
        if not r:
            print(json.dumps({"file": f, "page": i + 1, "error": "no reference"})); continue
        try:
            out = m.compare(pb, r[0], 8, pa)
        except Exception as e:
            print(json.dumps({"file": f, "page": i + 1, "error": repr(e)[:100]})); continue
        out.update({"file": f, "page": i + 1})
        print(json.dumps(out), flush=True)
        shutil.rmtree(tmp + "/r%d" % i)
