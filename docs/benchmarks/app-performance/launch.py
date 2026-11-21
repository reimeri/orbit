"""Run inside nix develop at the repository root, after building release orbit
and the orbit-core performance example. All state and traces stay under /tmp.
"""
import argparse
from contextlib import contextmanager
import json
import os
from pathlib import Path
import re
import shutil
import sqlite3
import subprocess
import tempfile
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--hardware-icd", type=Path,
                    help="Vulkan ICD for an installed GPU; uses Weston GL and a private shader cache")
args = parser.parse_args()
root = Path(tempfile.mkdtemp(prefix="orbit-launch-", dir="/tmp"))
print(f"Traces and fixture: {root}", flush=True)
exe = Path("target/release/orbit").resolve()
env = dict(os.environ, XDG_RUNTIME_DIR=str(root), WAYLAND_DISPLAY="orbit-audit",
           WGPU_BACKEND="vulkan", ICED_BACKEND="wgpu", MESA_SHADER_CACHE_DISABLE="true",
           VK_DRIVER_FILES=os.environ["ORBIT_TEST_VULKAN_ICD"], WAYLAND_DEBUG="client")
if args.hardware_icd:
    env.update(VK_DRIVER_FILES=str(args.hardware_icd.resolve(strict=True)),
               XDG_CACHE_HOME=str(root / "cache"))
    env.pop("MESA_SHADER_CACHE_DISABLE")
for key in ("DISPLAY", "WAYLAND_SOCKET", "VK_ICD_FILENAMES"):
    env.pop(key, None)

cases = ["empty", "sessions", "assignments"]
def database(case):
    path = root / case / "orbit" / "sessions.sqlite3"
    path.parent.mkdir(parents=True)
    return path

full = database("assignments")
with (root / "fixture.log").open("w") as log:
    subprocess.run(["target/release/examples/performance"],
                   env=dict(os.environ, ORBIT_BENCH_DB_COPY=str(full)),
                   stdout=log, stderr=subprocess.STDOUT, check=True)
base = shutil.copyfile(full, database("sessions"))
with sqlite3.connect(base) as db:
    db.execute("DELETE FROM subagent_runs")
    db.commit()
    db.execute("VACUUM")

def ticks(pid):
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return int(fields[11]) + int(fields[12])

def launch(case, sample):
    trace = root / f"{case}-{sample}.log"
    with trace.open("wb") as log:
        start = time.monotonic()
        with subprocess.Popen([exe], cwd=root, stdout=log, stderr=log,
                              env=dict(env, XDG_CONFIG_HOME=str(root / case))) as app:
            try:
                with trace.open("rb") as stream:
                    surface = None
                    attached = False
                    while time.monotonic() - start < 30 and app.poll() is None:
                        line = stream.readline()
                        if not line:
                            time.sleep(.001)
                        match = re.search(rb'get_xdg_surface\([^,]*, (wl_surface[#@]\d+)\)', line)
                        if match:
                            surface = match[1]
                        if surface and surface + b".attach(wl_buffer" in line:
                            attached = True
                        if attached and surface + b".commit(" in line:
                            break
                    else:
                        raise RuntimeError(f"No buffer commit: inspect {trace}")
                commit_ms = 1000 * (time.monotonic() - start)
                time.sleep(1)
                pid = app.pid
                then, cpu = time.monotonic(), ticks(pid)
                time.sleep(5)
                cpu_percent = (ticks(pid) - cpu) * 100 / os.sysconf("SC_CLK_TCK") / (time.monotonic() - then)
                if app.poll() is not None:
                    raise RuntimeError(f"Window exited while sampling: inspect {trace}")
                mem = Path(f"/proc/{pid}/smaps_rollup").read_text()
                mib = {k: int(re.search(rf"^{k}:\s+(\d+) kB", mem, re.M)[1]) / 1024 for k in ("Pss", "Rss")}
                print(json.dumps(dict(case=case, sample=sample, commit_ms=commit_ms,
                                      idle_cpu_percent=cpu_percent, **mib)), flush=True)
            finally:
                app.terminate()

@contextmanager
def compositor(name):
    renderer = "gl" if args.hardware_icd else "pixman"
    env["WAYLAND_DISPLAY"] = name
    with subprocess.Popen(["weston", "--backend=headless", f"--renderer={renderer}", "--shell=kiosk",
                           "--width=1200", "--height=800", "--idle-time=0", "--no-config",
                           f"--socket={name}", f"--log={root}/{name}.log"], env=env) as weston:
        try:
            for _ in range(100):
                if (root / name).is_socket():
                    break
                time.sleep(.05)
            else:
                raise RuntimeError(f"Compositor failed to start: {root}/{name}.log")
            yield
        finally:
            weston.terminate()

# Rotate order; discard round zero. Isolate GL compositor lifetimes because
# clients are terminated without draining their outstanding presentation events.
runs = [(cases[(i + j) % len(cases)], i) for i in range(4) for j in range(len(cases))]
if args.hardware_icd:
    for case, sample in runs:
        with compositor(f"orbit-{case}-{sample}"):
            launch(case, sample)
else:
    with compositor("orbit-audit"):
        for case, sample in runs:
            launch(case, sample)
