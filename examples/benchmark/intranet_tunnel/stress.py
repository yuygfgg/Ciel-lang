#!/usr/bin/env python3
import argparse
import os
import re
import shlex
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path


SCRIPT_DIR = Path(__file__).resolve().parent
ROOT = SCRIPT_DIR.parents[2]
EXAMPLES_ROOT = ROOT / "examples"
STRESS_TOOL = SCRIPT_DIR / "stress_tool"
GO_TUNNEL = EXAMPLES_ROOT / "intranet_tunnel_go"
CIEL_TUNNEL = EXAMPLES_ROOT / "intranet_tunnel"
PSK = "stress-secret-key"
ROUTE = "dev"


@dataclass
class Implementation:
    name: str
    server: Path
    agent: Path
    server_marker: str
    agent_marker: str


@dataclass
class Trial:
    success: bool
    metrics: dict
    output: str
    timed_out: bool = False


@dataclass
class LimitResult:
    name: str
    max_concurrency: int
    first_failure: int | None
    capped: bool
    boundary_trial: Trial | None
    peak_trial: Trial | None


@dataclass
class LoadRunner:
    tool: Path
    args: argparse.Namespace
    last_large_trial_end: float | None = None

    def run(self, addr: str, concurrency: int, label: str) -> Trial:
        large_trial = self.is_large_trial(concurrency)
        if (
            large_trial
            and self.last_large_trial_end is not None
            and self.args.trial_gap_ms > 0
        ):
            min_gap = self.args.trial_gap_ms / 1000.0
            elapsed = time.monotonic() - self.last_large_trial_end
            remaining = min_gap - elapsed
            if remaining > 0:
                print(
                    f"cooldown: waiting {remaining:.1f}s before "
                    f"high-concurrency {label} concurrency={concurrency}",
                    flush=True,
                )
                time.sleep(remaining)
        trial = run_load_trial(self.tool, addr, concurrency, self.args)
        if large_trial:
            self.last_large_trial_end = time.monotonic()
        return trial

    def is_large_trial(self, concurrency: int) -> bool:
        return concurrency >= self.args.trial_gap_threshold


def main() -> int:
    args = parse_args()
    validate_args(args)
    configure_fd_limit(args.fd_limit)

    work_dir = Path(tempfile.mkdtemp(prefix="ciel_tunnel_stress_"))
    success = False
    processes = []
    try:
        tool = compile_stress_tool(work_dir)
        load_runner = LoadRunner(tool, args)
        implementations = build_implementations(args, work_dir)

        echo_log = work_dir / "echo.log"
        echo = start_process(
            [str(tool), "echo-server", "--bind", "127.0.0.1:0"],
            echo_log,
        )
        processes.append(echo)
        echo_addr = wait_for_echo_addr(echo_log, echo)
        print(f"echo target: {echo_addr}", flush=True)

        if not args.skip_baseline:
            print(
                f"baseline: validating load rig at concurrency={args.ceiling}",
                flush=True,
            )
            baseline = load_runner.run(echo_addr, args.ceiling, "baseline")
            print_trial("baseline", args.ceiling, baseline)
            if not baseline.success:
                raise RuntimeError(
                    "direct echo baseline failed; lower --ceiling, raise host limits, "
                    "or use --skip-baseline only if another bottleneck check is in place"
                )

        results = []
        for impl in implementations:
            result = run_implementation(
                impl, load_runner, echo_addr, work_dir, args
            )
            results.append(result)

        print_summary(results)
        success = True
        return 0
    except KeyboardInterrupt:
        print("interrupted", file=sys.stderr)
        return 130
    except Exception as error:
        print(f"stress test failed: {error}", file=sys.stderr)
        return 1
    finally:
        for proc in processes:
            stop_process(proc)
        if args.keep_workdir or not success:
            print(f"work dir: {work_dir}", file=sys.stderr)
        else:
            shutil.rmtree(work_dir, ignore_errors=True)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Compare the Ciel and Go intranet tunnel implementations under "
            "the same compiled Rust load generator."
        )
    )
    parser.add_argument(
        "--only",
        choices=["all", "go", "ciel"],
        default="all",
        help="implementation set to benchmark",
    )
    parser.add_argument(
        "--ceiling",
        type=int,
        default=128,
        help="maximum concurrency searched by the binary search",
    )
    parser.add_argument(
        "--payload-bytes",
        type=int,
        default=65536,
        help="bytes written by each client per round trip",
    )
    parser.add_argument(
        "--round-trips",
        type=int,
        default=8,
        help="round trips each client must complete in every trial",
    )
    parser.add_argument(
        "--timeout-ms",
        type=int,
        default=15000,
        help="per-socket timeout used by the compiled load generator",
    )
    parser.add_argument(
        "--trial-timeout-ms",
        type=int,
        default=30000,
        help="wall-clock timeout for one load generator process",
    )
    parser.add_argument(
        "--trial-gap-ms",
        type=int,
        default=15000,
        help=(
            "minimum delay between high-concurrency load generator trials; "
            "use 0 to disable"
        ),
    )
    parser.add_argument(
        "--trial-gap-threshold",
        type=int,
        default=512,
        help="only trials at or above this concurrency use --trial-gap-ms",
    )
    parser.add_argument(
        "--skip-baseline",
        action="store_true",
        help="skip the direct echo baseline that checks the load rig capacity",
    )
    parser.add_argument(
        "--keep-workdir",
        action="store_true",
        help="keep temporary binaries and logs after a successful run",
    )
    parser.add_argument(
        "--fd-limit",
        type=int,
        default=65536,
        help="soft RLIMIT_NOFILE requested before spawning benchmark processes; use 0 to skip",
    )
    return parser.parse_args()


def validate_args(args: argparse.Namespace) -> None:
    for name in [
        "ceiling",
        "payload_bytes",
        "round_trips",
        "timeout_ms",
        "trial_timeout_ms",
        "trial_gap_ms",
        "trial_gap_threshold",
        "fd_limit",
    ]:
        if (
            name in ("fd_limit", "trial_gap_ms", "trial_gap_threshold")
            and getattr(args, name) == 0
        ):
            continue
        if getattr(args, name) <= 0:
            raise SystemExit(f"--{name.replace('_', '-')} must be greater than zero")


def configure_fd_limit(requested: int) -> None:
    if requested == 0:
        return
    try:
        import resource
    except ImportError:
        print("fd limit: resource module unavailable; skipping", file=sys.stderr)
        return

    soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
    if soft >= requested:
        print(f"fd limit: soft={soft} hard={hard}", flush=True)
        return

    new_hard = hard
    if hard != resource.RLIM_INFINITY and hard < requested and hasattr(os, "geteuid") and os.geteuid() == 0:
        new_hard = requested
    target = requested
    if new_hard != resource.RLIM_INFINITY:
        target = min(requested, new_hard)

    try:
        resource.setrlimit(resource.RLIMIT_NOFILE, (target, new_hard))
    except (OSError, ValueError) as error:
        print(
            f"fd limit: failed to raise soft limit from {soft} to {requested}: {error}",
            file=sys.stderr,
        )
        return

    new_soft, new_hard = resource.getrlimit(resource.RLIMIT_NOFILE)
    print(f"fd limit: soft={new_soft} hard={new_hard}", flush=True)
    if new_soft < requested:
        print(
            f"fd limit: requested {requested}, but only {new_soft} is available",
            file=sys.stderr,
        )


def compile_stress_tool(work_dir: Path) -> Path:
    target_dir = work_dir / "stress-tool-target"
    env = dict(os.environ)
    env["CARGO_TARGET_DIR"] = str(target_dir)
    run_checked(
        [
            "cargo",
            "build",
            "--quiet",
            "--release",
            "--locked",
        ],
        cwd=STRESS_TOOL,
        env=env,
    )
    return target_dir / "release" / exe_name("stress-tool")


def build_implementations(args: argparse.Namespace, work_dir: Path) -> list[Implementation]:
    selected = []
    if args.only in ("all", "go"):
        selected.append(build_go_implementation(work_dir))
    if args.only in ("all", "ciel"):
        selected.append(build_ciel_implementation(work_dir))
    return selected


def build_go_implementation(work_dir: Path) -> Implementation:
    server = work_dir / exe_name("go-tunnel-server")
    agent = work_dir / exe_name("go-tunnel-agent")
    env = dict(os.environ)
    env.setdefault("GOCACHE", str(work_dir / "go-build-cache"))
    run_checked(
        [
            "go",
            "build",
            "-C",
            str(GO_TUNNEL),
            "-o",
            str(server),
            "./cmd/tunnel-server",
        ],
        env=env,
    )
    run_checked(
        [
            "go",
            "build",
            "-C",
            str(GO_TUNNEL),
            "-o",
            str(agent),
            "./cmd/tunnel-agent",
        ],
        env=env,
    )
    return Implementation(
        name="go",
        server=server,
        agent=agent,
        server_marker="server started:",
        agent_marker="agent connected to server",
    )


def build_ciel_implementation(work_dir: Path) -> Implementation:
    server = work_dir / exe_name("ciel-tunnel-server")
    agent = work_dir / exe_name("ciel-tunnel-agent")
    compile_ciel("server", server)
    compile_ciel("agent", agent)
    return Implementation(
        name="ciel",
        server=server,
        agent=agent,
        server_marker="tunnel-server: ready",
        agent_marker="tunnel-agent: authenticated",
    )


def compile_ciel(entry: str, output: Path) -> None:
    cielc = os.environ.get("CIELC")
    if cielc:
        cmd = [
            cielc,
            "--release",
            "--manifest-path",
            str(CIEL_TUNNEL / "ciel.toml"),
            "--std-path",
            str(ROOT),
            "--entry",
            entry,
            "-o",
            str(output),
        ]
    else:
        cmd = [
            "cargo",
            "run",
            "--quiet",
            "--release",
            "--manifest-path",
            str(ROOT / "Cargo.toml"),
            "--",
            "--release",
            "--manifest-path",
            str(CIEL_TUNNEL / "ciel.toml"),
            "--std-path",
            str(ROOT),
            "--entry",
            entry,
            "-o",
            str(output),
        ]
    run_checked(cmd)


def run_implementation(
    impl: Implementation,
    load_runner: LoadRunner,
    echo_addr: str,
    work_dir: Path,
    args: argparse.Namespace,
) -> LimitResult:
    control_addr = f"127.0.0.1:{free_port()}"
    public_addr = f"127.0.0.1:{free_port()}"
    server_log = work_dir / f"{impl.name}-server.log"
    agent_log = work_dir / f"{impl.name}-agent.log"
    server = None
    agent = None
    try:
        print(f"\n=== {impl.name} ===", flush=True)
        server = start_process(
            [
                str(impl.server),
                "--control",
                control_addr,
                "--public",
                public_addr,
                "--route",
                ROUTE,
                "--psk",
                PSK,
            ],
            server_log,
        )
        wait_for_log(server_log, impl.server_marker, server)

        agent = start_process(
            [
                str(impl.agent),
                "--server",
                control_addr,
                "--target",
                echo_addr,
                "--route",
                ROUTE,
                "--psk",
                PSK,
            ],
            agent_log,
        )
        wait_for_log(agent_log, impl.agent_marker, agent)

        return find_limit(impl.name, load_runner, public_addr, args)
    except Exception:
        dump_logs([(f"{impl.name} server", server_log), (f"{impl.name} agent", agent_log)])
        raise
    finally:
        stop_process(agent)
        stop_process(server)


def find_limit(
    name: str,
    load_runner: LoadRunner,
    public_addr: str,
    args: argparse.Namespace,
) -> LimitResult:
    last_good = None
    peak_trial = None
    low = 0
    high = None
    current = 1

    while True:
        target = min(current, args.ceiling)
        trial = load_runner.run(public_addr, target, name)
        print_trial(name, target, trial)
        if trial.success:
            last_good = trial
            peak_trial = choose_peak(peak_trial, trial)
            low = target
            if target == args.ceiling:
                return LimitResult(name, low, None, True, last_good, peak_trial)
            current = min(target * 2, args.ceiling)
        else:
            high = target
            break

    while low + 1 < high:
        mid = (low + high) // 2
        trial = load_runner.run(public_addr, mid, name)
        print_trial(name, mid, trial)
        if trial.success:
            low = mid
            last_good = trial
            peak_trial = choose_peak(peak_trial, trial)
        else:
            high = mid

    return LimitResult(name, low, high, False, last_good, peak_trial)


def choose_peak(current: Trial | None, candidate: Trial) -> Trial:
    if current is None:
        return candidate
    if candidate.metrics.get("mibps", 0.0) > current.metrics.get("mibps", 0.0):
        return candidate
    return current


def run_load_trial(
    tool: Path,
    addr: str,
    concurrency: int,
    args: argparse.Namespace,
) -> Trial:
    cmd = [
        str(tool),
        "loadgen",
        "--addr",
        addr,
        "--concurrency",
        str(concurrency),
        "--payload-bytes",
        str(args.payload_bytes),
        "--round-trips",
        str(args.round_trips),
        "--timeout-ms",
        str(args.timeout_ms),
    ]
    try:
        completed = subprocess.run(
            cmd,
            cwd=ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=args.trial_timeout_ms / 1000.0,
        )
    except subprocess.TimeoutExpired as error:
        output = (error.stdout or "") + (error.stderr or "")
        return Trial(False, {}, output + "\nTIMEOUT", timed_out=True)

    output = completed.stdout + completed.stderr
    status, metrics = parse_metrics(output)
    return Trial(completed.returncode == 0 and status == "OK", metrics, output)


def parse_metrics(output: str) -> tuple[str | None, dict]:
    for line in output.splitlines():
        if line.startswith("OK ") or line.startswith("FAIL "):
            parts = line.split()
            metrics = {}
            for item in parts[1:]:
                if "=" not in item:
                    continue
                key, value = item.split("=", 1)
                try:
                    metrics[key] = float(value) if "." in value else int(value)
                except ValueError:
                    metrics[key] = value
            return parts[0], metrics
    return None, {}


def print_trial(name: str, concurrency: int, trial: Trial) -> None:
    if trial.success:
        mibps = trial.metrics.get("mibps", 0.0)
        rps = trial.metrics.get("rps", 0.0)
        elapsed = trial.metrics.get("elapsed_ms", 0.0)
        print(
            f"{name}: concurrency={concurrency} ok "
            f"mibps={mibps:.1f} rps={rps:.1f} elapsed_ms={elapsed:.0f}",
            flush=True,
        )
    else:
        reason = "timeout" if trial.timed_out else first_output_line(trial.output)
        print(f"{name}: concurrency={concurrency} fail {reason}", flush=True)


def print_summary(results: list[LimitResult]) -> None:
    print("\n=== Summary ===")
    print(
        "implementation max_concurrency first_failure capped "
        "boundary_mibps boundary_rps peak_concurrency peak_mibps peak_rps"
    )
    for result in results:
        boundary = result.boundary_trial.metrics if result.boundary_trial else {}
        peak = result.peak_trial.metrics if result.peak_trial else {}
        print(
            f"{result.name} "
            f"{result.max_concurrency} "
            f"{result.first_failure if result.first_failure is not None else '-'} "
            f"{'yes' if result.capped else 'no'} "
            f"{boundary.get('mibps', 0.0):.1f} "
            f"{boundary.get('rps', 0.0):.1f} "
            f"{peak.get('concurrency', 0)} "
            f"{peak.get('mibps', 0.0):.1f} "
            f"{peak.get('rps', 0.0):.1f}"
        )

    by_name = {result.name: result for result in results}
    go = by_name.get("go")
    ciel = by_name.get("ciel")
    if go and ciel and go.max_concurrency > 0:
        ratio = ciel.max_concurrency / go.max_concurrency
        print(f"ciel/go concurrency ratio: {ratio:.3f}")

    for result in results:
        if result.capped:
            print(
                f"{result.name}: reached --ceiling; raise --ceiling for a higher bound",
                file=sys.stderr,
            )


def start_process(args: list[str], log_path: Path) -> subprocess.Popen:
    log = open(log_path, "ab", buffering=0)
    proc = subprocess.Popen(args, stdout=log, stderr=subprocess.STDOUT, cwd=ROOT)
    proc._stress_log = log
    return proc


def stop_process(proc: subprocess.Popen | None) -> None:
    if proc is None:
        return
    if proc.poll() is None:
        proc.terminate()
        try:
            proc.wait(timeout=2.0)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=2.0)
    log = getattr(proc, "_stress_log", None)
    if log is not None:
        log.close()


def wait_for_echo_addr(log_path: Path, proc: subprocess.Popen, timeout: float = 8.0) -> str:
    pattern = re.compile(r"READY echo-server addr=([^\s]+)")
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            raise RuntimeError("echo server exited before it became ready")
        text = read_text(log_path)
        match = pattern.search(text)
        if match:
            return match.group(1)
        time.sleep(0.05)
    raise RuntimeError("timed out waiting for echo server")


def wait_for_log(
    log_path: Path,
    marker: str,
    proc: subprocess.Popen,
    timeout: float = 12.0,
) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            raise RuntimeError(f"process exited before log marker {marker!r}")
        if marker in read_text(log_path):
            return
        time.sleep(0.05)
    raise RuntimeError(f"timed out waiting for log marker {marker!r}")


def dump_logs(paths: list[tuple[str, Path]]) -> None:
    for label, path in paths:
        print(f"--- {label} log ---", file=sys.stderr)
        text = read_text(path)
        if not text:
            print("(empty)", file=sys.stderr)
            continue
        lines = text.splitlines()
        for line in lines[-120:]:
            print(line, file=sys.stderr)


def read_text(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8", errors="replace")
    except FileNotFoundError:
        return ""


def run_checked(
    cmd: list[str],
    env: dict[str, str] | None = None,
    cwd: Path = ROOT,
) -> None:
    print("+ " + " ".join(shlex.quote(part) for part in cmd), flush=True)
    subprocess.run(cmd, cwd=cwd, check=True, env=env)


def exe_name(stem: str) -> str:
    return stem + ".exe" if os.name == "nt" else stem


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def first_output_line(output: str) -> str:
    for line in output.splitlines():
        if line.strip():
            return line.strip()
    return "no output"


if __name__ == "__main__":
    sys.exit(main())
