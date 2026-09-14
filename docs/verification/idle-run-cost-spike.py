#!/usr/bin/env python3
#
# What does an idle live run cost, and what would closing it cost instead?
#
# `driver::Registry` holds a `claude` process open for the life of a
# conversation -- nothing closes stdin on a run's behalf -- and the home screen
# is a canvas of conversations, so the number of held processes is the number of
# columns on screen. The alternative is to close an idle run and let
# `send_turn`'s resume path wake it, which has worked at the CLI level only
# since `f5ba282`. This measures both sides of that trade rather than arguing
# it:
#
#   A  what an idle post-turn `claude` costs: RSS/PSS/USS, fds, threads, and
#      the marginal cost of the fifth one rather than an extrapolation of one
#   B  prompt-to-first-token on a live process versus on a resume, repeated,
#      because one sample of a network-bound number is not a number
#   C  whether repeated close/resume cycles are lossless: the session id, the
#      conversation, and -- the one our log depends on -- whether a resumed
#      stream replays history, which is what `Pipeline::resuming`'s
#      duplicate-detection path is for
#
# Prints rather than asserts: these are numbers, not invariants, and the
# thresholds worth asserting are not knowable before the first run.
#
#   python3 docs/verification/idle-run-cost-spike.py
#
# Env: CLAUDE_BIN (default: claude), MODEL (default: claude-haiku-4-5-20251001),
#      WORK (default: ~/.cache/aw-idle-run-cost)
#
# The sandbox is on real storage and NOT in the scratchpad, which is tmpfs here.
# Nothing below fsyncs on purpose, but a resume re-reads a transcript and scans
# a project directory, and measuring that against a RAM filesystem is the same
# mistake `event-log/examples/append_cost.rs` records having made once already.

import json
import os
import shutil
import subprocess
import sys
import threading
import time
import uuid
from pathlib import Path

CLAUDE = os.environ.get("CLAUDE_BIN", "claude")
MODEL = os.environ.get("MODEL", "claude-haiku-4-5-20251001")
WORK = Path(os.environ.get("WORK", str(Path.home() / ".cache" / "aw-idle-run-cost")))
SANDBOX = WORK / "sandbox"
CODEWORD = "marmalade-7731"


def sh(*args, cwd=None):
    return subprocess.run(args, cwd=cwd, capture_output=True, text=True, check=True).stdout


def setup():
    if WORK.exists():
        shutil.rmtree(WORK)
    SANDBOX.mkdir(parents=True)
    sh("git", "init", "-q", "-b", "main", cwd=SANDBOX)
    sh("git", "config", "user.email", "fixtures@example.com", cwd=SANDBOX)
    sh("git", "config", "user.name", "Fixture Recorder", cwd=SANDBOX)
    (SANDBOX / "README.md").write_text("placeholder\n")
    sh("git", "add", "-A", cwd=SANDBOX)
    sh("git", "commit", "-qm", "init", cwd=SANDBOX)
    fs = subprocess.run(
        ["df", "--output=fstype", str(SANDBOX)], capture_output=True, text=True
    ).stdout.splitlines()[1].strip()
    print(f"claude:  {subprocess.run([CLAUDE, '--version'], capture_output=True, text=True).stdout.strip()}")
    print(f"model:   {MODEL}")
    print(f"sandbox: {SANDBOX} ({fs})")
    if fs == "tmpfs":
        print("  WARNING: tmpfs. Resume latency here is not the latency a user pays.")
    print()


# --- one agent process, driven the way the driver drives it -------------------


def argv(session_id=None, resume=None, max_turns=8):
    """`LaunchSpec::argv` reproduced faithfully, minus the workspace MCP flags
    (no server here) and with the two resuming shapes `f5ba282` settled."""
    a = [
        CLAUDE,
        "--output-format", "stream-json", "--verbose",
        "--input-format", "stream-json", "--replay-user-messages",
        "--strict-mcp-config",
    ]
    if resume is None:
        a += ["--session-id", session_id]
    else:
        a += ["--resume", resume]
    a += [
        "--thinking-display", "summarized",
        "--model", MODEL,
        "--permission-prompts", "none",
        "--max-turns", str(max_turns),
        "--max-budget-usd", "0.05",
        "-p",
    ]
    return a


class Agent:
    """A live run: a process, a reader thread, and timestamped lines."""

    def __init__(self, label, session_id=None, resume=None):
        self.label = label
        self.lines = []  # (monotonic, parsed json or raw str)
        self.lock = threading.Lock()
        self.t0 = time.monotonic()
        self.proc = subprocess.Popen(
            argv(session_id, resume),
            cwd=SANDBOX,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()
        self.err = []
        self.errreader = threading.Thread(target=self._read_err, daemon=True)
        self.errreader.start()

    def _read(self):
        for line in self.proc.stdout:
            t = time.monotonic()
            try:
                v = json.loads(line)
            except json.JSONDecodeError:
                v = line.rstrip("\n")
            with self.lock:
                self.lines.append((t, v))

    def _read_err(self):
        for line in self.proc.stderr:
            self.err.append(line.rstrip("\n"))

    def send(self, text):
        msg = {
            "type": "user",
            "message": {"role": "user", "content": [{"type": "text", "text": text}]},
            "parent_tool_use_id": None,
        }
        self.proc.stdin.write(json.dumps(msg) + "\n")
        self.proc.stdin.flush()
        return time.monotonic()

    def snapshot(self):
        with self.lock:
            return list(self.lines)

    def wait_for(self, pred, timeout=120, since=0):
        """Block until a line at index >= `since` satisfies `pred`."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            for i, (t, v) in enumerate(self.snapshot()):
                if i >= since and isinstance(v, dict) and pred(v):
                    return i, t, v
            if self.proc.poll() is not None and not self.reader.is_alive():
                raise RuntimeError(
                    f"{self.label}: exited ({self.proc.returncode}) before the line arrived; "
                    f"stderr: {' | '.join(self.err[-4:])}"
                )
            time.sleep(0.005)
        raise TimeoutError(f"{self.label}: nothing matched in {timeout}s")

    def turn(self, text, timeout=120):
        """Send, then wait for `result`. Returns (sent_at, first_line_at,
        first_assistant_text_at, result_at, result)."""
        since = len(self.snapshot())
        sent = self.send(text)
        i, t_first, _ = self.wait_for(lambda v: True, timeout, since)
        _, t_text, _ = self.wait_for(is_assistant_text, timeout, since)
        _, t_result, res = self.wait_for(lambda v: v.get("type") == "result", timeout, since)
        return sent, t_first, t_text, t_result, res

    def close(self):
        try:
            self.proc.stdin.close()
        except Exception:
            pass
        try:
            self.proc.wait(timeout=60)
        except subprocess.TimeoutExpired:
            self.proc.kill()

    def kill(self):
        self.proc.kill()
        self.proc.wait(timeout=30)


def is_assistant_text(v):
    if v.get("type") != "assistant":
        return False
    for b in v.get("message", {}).get("content", []):
        if b.get("type") == "text" and b.get("text"):
            return True
    return False


def reply_text(agent, since=0):
    out = []
    for i, (_, v) in enumerate(agent.snapshot()):
        if i >= since and isinstance(v, dict) and v.get("type") == "assistant":
            for b in v.get("message", {}).get("content", []):
                if b.get("type") == "text":
                    out.append(b["text"])
    return " ".join(out)


# --- /proc ---------------------------------------------------------------


def children(pid):
    out = []
    for p in Path("/proc").iterdir():
        if not p.name.isdigit():
            continue
        try:
            stat = (p / "stat").read_text()
            ppid = int(stat[stat.rindex(")") + 2 :].split()[1])
        except (OSError, ValueError):
            continue
        if ppid == pid:
            out.append(int(p.name))
    return out


def mem(pid):
    """kB, from smaps_rollup: the honest set for a Node process."""
    fields = {}
    try:
        for line in Path(f"/proc/{pid}/smaps_rollup").read_text().splitlines():
            if ":" in line:
                k, v = line.split(":", 1)
                v = v.strip().split()
                if v and v[0].isdigit():
                    fields[k] = int(v[0])
    except OSError:
        return None
    return {
        "rss": fields.get("Rss", 0),
        "pss": fields.get("Pss", 0),
        "uss": fields.get("Private_Clean", 0) + fields.get("Private_Dirty", 0),
        "shared": fields.get("Shared_Clean", 0) + fields.get("Shared_Dirty", 0),
    }


def fds(pid):
    try:
        return len(os.listdir(f"/proc/{pid}/fd"))
    except OSError:
        return -1


def threads(pid):
    try:
        for line in Path(f"/proc/{pid}/status").read_text().splitlines():
            if line.startswith("Threads:"):
                return int(line.split()[1])
    except OSError:
        pass
    return -1


def report(agents):
    tot = {"rss": 0, "pss": 0, "uss": 0}
    print(f"  {'run':<10} {'pid':>8} {'RSS':>9} {'PSS':>9} {'USS':>9} {'shared':>9} {'fds':>5} {'thr':>5} {'kids':>5}")
    for a in agents:
        pid = a.proc.pid
        m = mem(pid)
        if m is None:
            print(f"  {a.label:<10} {pid:>8}  gone")
            continue
        for k in tot:
            tot[k] += m[k]
        print(
            f"  {a.label:<10} {pid:>8} {m['rss']/1024:>8.1f}M {m['pss']/1024:>8.1f}M "
            f"{m['uss']/1024:>8.1f}M {m['shared']/1024:>8.1f}M {fds(pid):>5} "
            f"{threads(pid):>5} {len(children(pid)):>5}"
        )
    print(
        f"  {'TOTAL':<10} {len(agents):>8} {tot['rss']/1024:>8.1f}M {tot['pss']/1024:>8.1f}M "
        f"{tot['uss']/1024:>8.1f}M"
    )
    return tot


def ms(a, b):
    return (b - a) * 1000.0


def stats(xs):
    xs = sorted(xs)
    n = len(xs)
    return {
        "min": xs[0],
        "median": xs[n // 2],
        "max": xs[-1],
        "mean": sum(xs) / n,
    }


def line(label, xs):
    s = stats(xs)
    print(
        f"  {label:<34} n={len(xs)}  min {s['min']:>7.0f}ms  median {s['median']:>7.0f}ms  "
        f"mean {s['mean']:>7.0f}ms  max {s['max']:>7.0f}ms"
    )


# --- A: what an idle live run costs -------------------------------------------


def part_a():
    print("A. What an idle live run costs")
    print()
    agents = []
    sids = []
    for i in range(5):
        sid = str(uuid.uuid4())
        sids.append(sid)
        a = Agent(f"run{i}", session_id=sid)
        a.turn("Reply with exactly: ok")
        agents.append(a)
        if i == 0:
            time.sleep(2)  # let the turn's allocations settle
            print("  one idle run, just after its first turn finished:")
            one = report([a])
            print()
    time.sleep(2)
    print("  five idle runs, each one turn in:")
    five = report(agents)
    print()
    print(f"  marginal RSS of runs 2..5: {(five['rss'] - one['rss'])/4/1024:.1f}M each")
    print(f"  marginal PSS of runs 2..5: {(five['pss'] - one['pss'])/4/1024:.1f}M each")
    print(f"  sum of RSS double-counts {(five['rss'] - five['pss'])/1024:.1f}M of shared pages")
    print()
    return agents, sids, one, five


# --- B: resume latency versus a live send -------------------------------------


def part_b(live):
    print("B. What a resume costs")
    print()
    # Its own session, closed before it is resumed: a session another process
    # is holding is not a thing we may resume, and the case being measured is
    # precisely a run that was closed and has to be woken.
    sid = str(uuid.uuid4())
    seed = Agent("seed", session_id=sid)
    seed.turn("Reply with exactly: ok")
    seed.close()

    live_first, live_text, live_result = [], [], []
    for i in range(5):
        sent, t_first, t_text, t_result, _ = live.turn(f"Reply with exactly: live-{i}")
        live_first.append(ms(sent, t_first))
        live_text.append(ms(sent, t_text))
        live_result.append(ms(sent, t_result))
    line("live: prompt -> first event", live_first)
    line("live: prompt -> first text", live_text)
    line("live: prompt -> result", live_result)
    print()

    res_first, res_text, res_result, exits = [], [], [], []
    for i in range(5):
        a = Agent(f"res{i}", resume=sid)
        spawn = a.t0
        a.send(f"Reply with exactly: resumed-{i}")
        _, t_first, _ = a.wait_for(lambda v: True)
        _, t_text, _ = a.wait_for(is_assistant_text)
        _, t_result, _ = a.wait_for(lambda v: v.get("type") == "result")
        res_first.append(ms(spawn, t_first))
        res_text.append(ms(spawn, t_text))
        res_result.append(ms(spawn, t_result))
        t_close = time.monotonic()
        a.close()
        exits.append(ms(t_close, time.monotonic()))
    line("resume: spawn -> first event", res_first)
    line("resume: spawn -> first text", res_text)
    line("resume: spawn -> result", res_result)
    line("resume: stdin close -> exit", exits)
    print()
    print(
        f"  the user pays {stats(res_text)['median'] - stats(live_text)['median']:+.0f}ms "
        f"at the median on first text, and "
        f"{stats(res_first)['median'] - stats(live_first)['median']:+.0f}ms before anything appears"
    )
    print()


# --- C: is a close/resume cycle lossless? -------------------------------------


def part_c():
    print("C. Repeated close/resume cycles")
    print()
    sid = str(uuid.uuid4())
    a = Agent("cyc", session_id=sid)
    a.turn(f"Remember this codeword: {CODEWORD}. Reply with exactly: stored")
    init = next(v for _, v in a.snapshot() if isinstance(v, dict) and v.get("type") == "system")
    print(f"  fresh session id:  {init.get('session_id')}  (asked for {sid})")
    trunk_uuids = [v.get("uuid") for _, v in a.snapshot() if isinstance(v, dict) and v.get("uuid")]
    print(f"  trunk emitted {len(trunk_uuids)} lines carrying a uuid")
    a.close()
    print(f"  closed; exit {a.proc.returncode}")
    print()

    transcript = None
    for cycle in range(3):
        a = Agent(f"cyc{cycle}", resume=sid)
        a.send("What was the codeword? Reply with only the codeword.")
        _, _, init = a.wait_for(lambda v: v.get("type") == "system")
        before_turn = [
            v for _, v in a.snapshot() if isinstance(v, dict) and v.get("type") in ("user", "assistant")
        ]
        _, _, res = a.wait_for(lambda v: v.get("type") == "result")
        reply = reply_text(a)
        seen = [v.get("uuid") for _, v in a.snapshot() if isinstance(v, dict) and v.get("uuid")]
        replayed = [u for u in seen if u in trunk_uuids]
        print(f"  cycle {cycle}:")
        print(f"    init session_id      {init.get('session_id')}  same={init.get('session_id') == sid}")
        print(f"    result subtype       {res.get('subtype')}  session_id={res.get('session_id')}")
        print(f"    codeword survived    {CODEWORD in reply}   (reply: {reply.strip()[:60]!r})")
        print(f"    lines before the new turn started: {len(before_turn)}")
        print(f"    uuids seen {len(seen)}, of which already in the trunk: {len(replayed)}")
        trunk_uuids += [u for u in seen if u not in trunk_uuids]
        a.close()
        t = transcript_path(sid)
        size = t.stat().st_size if t and t.exists() else -1
        print(f"    exit {a.proc.returncode}; transcript {size} bytes"
              f"{'' if transcript is None else f' (+{size - transcript})'}")
        transcript = size
    print()
    print(f"  worktree intact: {(SANDBOX / 'README.md').exists()}; "
          f"git status clean: {sh('git', 'status', '--porcelain', cwd=SANDBOX).strip() == ''}")
    print()


def transcript_path(sid):
    root = Path.home() / ".claude" / "projects"
    if not root.exists():
        return None
    hits = list(root.glob(f"*/{sid}.jsonl"))
    return hits[0] if hits else None


def main():
    setup()
    agents, sids, one, five = part_a()
    try:
        part_b(agents[0])
    finally:
        for a in agents:
            a.kill()
    part_c()
    print("done. sandbox left at", SANDBOX)


if __name__ == "__main__":
    sys.exit(main())
