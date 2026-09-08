#!/usr/bin/env python3
"""Repeatable cold-build and loopback runtime performance audit.

The audit intentionally uses only the Python standard library and the
repository's deterministic mock provider. It never contacts a real provider.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parent.parent
DEFAULT_THRESHOLDS = ROOT / "performance" / "thresholds.json"
NO_PROXY_OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def percentile(values: list[float], percent: float) -> float:
    """Return a linearly interpolated percentile for a non-empty sample."""
    if not values:
        raise ValueError("percentile requires at least one value")
    ordered = sorted(values)
    position = (len(ordered) - 1) * percent / 100.0
    lower = int(position)
    upper = min(lower + 1, len(ordered) - 1)
    fraction = position - lower
    return ordered[lower] + (ordered[upper] - ordered[lower]) * fraction


def threshold_failures(
    metrics: dict[str, float | None], thresholds: dict[str, float]
) -> list[str]:
    failures = []
    for name, maximum in thresholds.items():
        value = metrics.get(name)
        if value is None:
            failures.append(f"{name}: metric was not recorded")
        elif value > maximum:
            failures.append(f"{name}: {value:.3f} > {maximum:.3f}")
    return failures


def available_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def command_output(*args: str) -> str:
    result = subprocess.run(
        args,
        cwd=ROOT,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
    )
    return result.stdout.strip() if result.returncode == 0 else "unknown"


def build_binary(target_dir: Path, release: bool) -> tuple[float, Path]:
    command = [
        "cargo",
        "build",
        "--locked",
        "--bin",
        "model-gateway",
        "--target-dir",
        str(target_dir),
    ]
    if release:
        command.append("--release")
    started = time.monotonic()
    result = subprocess.run(
        command,
        cwd=ROOT,
        env={**os.environ, "CARGO_INCREMENTAL": "0"},
        text=True,
    )
    elapsed = time.monotonic() - started
    if result.returncode != 0:
        raise RuntimeError(f"{' '.join(command)} failed with {result.returncode}")
    profile = "release" if release else "debug"
    binary = target_dir / profile / "model-gateway"
    if not binary.is_file():
        raise RuntimeError(f"build succeeded without producing {binary}")
    return elapsed, binary


def request_json(
    url: str,
    payload: dict[str, Any] | None = None,
    headers: dict[str, str] | None = None,
) -> dict[str, Any]:
    data = None if payload is None else json.dumps(payload).encode("utf-8")
    request = urllib.request.Request(
        url, data=data, method="GET" if data is None else "POST"
    )
    request.add_header("Content-Type", "application/json")
    for name, value in (headers or {}).items():
        request.add_header(name, value)
    with NO_PROXY_OPENER.open(request, timeout=5) as response:
        return json.load(response)


def wait_ready(
    url: str, process: subprocess.Popen[bytes], timeout: float = 10.0
) -> float:
    started = time.monotonic()
    deadline = started + timeout
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(
                f"gateway exited before readiness with {process.returncode}"
            )
        try:
            if request_json(url).get("status") == "ready":
                return (time.monotonic() - started) * 1000.0
        except (OSError, urllib.error.URLError, json.JSONDecodeError):
            pass
        time.sleep(0.01)
    raise RuntimeError(f"gateway did not become ready within {timeout:.1f}s")


def timed_requests(
    url: str,
    payload: dict[str, Any],
    samples: int,
    warmup: int,
    headers: dict[str, str] | None = None,
) -> list[float]:
    timings = []
    for index in range(warmup + samples):
        started = time.perf_counter()
        body = request_json(url, payload, headers)
        elapsed_ms = (time.perf_counter() - started) * 1000.0
        if body.get("model") != "upstream-smoke":
            raise RuntimeError(f"unexpected mock response model: {body.get('model')!r}")
        if index >= warmup:
            timings.append(elapsed_ms)
    return timings


def resident_memory_mib(pid: int) -> float:
    result = subprocess.run(
        ["ps", "-o", "rss=", "-p", str(pid)],
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    )
    return int(result.stdout.strip()) / 1024.0


def stop_process(process: subprocess.Popen[bytes] | None) -> None:
    if process is None or process.poll() is not None:
        return
    process.send_signal(signal.SIGTERM)
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


def runtime_metrics(binary: Path, samples: int, warmup: int) -> dict[str, float]:
    gateway_port = available_port()
    provider_port = available_port()
    with tempfile.TemporaryDirectory(prefix="model-gateway-perf-runtime-") as raw_state:
        state = Path(raw_state)
        home = state / "home"
        home.mkdir()
        config = state / "config.toml"
        config.write_text(
            f"""[server]
bind = "127.0.0.1:{gateway_port}"
exposure = "loopback"
max_body_bytes = 33554432
max_in_flight = 8
admission_timeout_ms = 250
shutdown_grace_seconds = 2

[providers.mock]
adapter = "openai_chat"
base_url = "http://127.0.0.1:{provider_port}/v1"
api_key_secret = "LOCAL_API_KEY"
allow_model_passthrough = false
allow_insecure_http = true
connect_timeout_seconds = 2
response_header_timeout_seconds = 5
stream_idle_timeout_seconds = 5

[models.smoke]
[[models.smoke.targets]]
provider = "mock"
model = "upstream-smoke"
""",
            encoding="utf-8",
        )
        provider_env = {**os.environ, "MOCK_PROVIDER_API_KEY": "fixture-secret"}
        provider = subprocess.Popen(
            [
                sys.executable,
                str(ROOT / "scripts" / "mock_provider.py"),
                str(provider_port),
            ],
            cwd=ROOT,
            env=provider_env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        gateway: subprocess.Popen[bytes] | None = None
        gateway_log_path = state / "gateway.log"
        try:
            provider_deadline = time.monotonic() + 5
            while time.monotonic() < provider_deadline:
                try:
                    request_json(f"http://127.0.0.1:{provider_port}/v1/models")
                    break
                except (OSError, urllib.error.URLError):
                    if provider.poll() is not None:
                        raise RuntimeError("mock provider exited before readiness")
                    time.sleep(0.01)
            else:
                raise RuntimeError("mock provider did not become ready")

            gateway_env = {
                **os.environ,
                "HOME": str(home),
                "MODEL_GATEWAY_CONFIG": str(config),
                "MODEL_GATEWAY_STATE_PATH": str(state / "routing.sqlite3"),
                "MODEL_GATEWAY_SECRET_STORE": "environment",
                "LOCAL_API_KEY": "fixture-secret",
                "NO_PROXY": "127.0.0.1,localhost",
                "no_proxy": "127.0.0.1,localhost",
                "HTTP_PROXY": "",
                "http_proxy": "",
                "HTTPS_PROXY": "",
                "https_proxy": "",
                "ALL_PROXY": "",
                "all_proxy": "",
            }
            with gateway_log_path.open("wb") as gateway_log:
                gateway = subprocess.Popen(
                    [str(binary), "serve"],
                    cwd=ROOT,
                    env=gateway_env,
                    stdout=gateway_log,
                    stderr=subprocess.STDOUT,
                )
                startup_ms = wait_ready(
                    f"http://127.0.0.1:{gateway_port}/health/ready", gateway
                )
                direct_payload = {
                    "model": "upstream-smoke",
                    "messages": [],
                    "tools": [{"type": "function", "function": {"name": "noop"}}],
                }
                gateway_payload = {**direct_payload, "model": "smoke"}
                direct = timed_requests(
                    f"http://127.0.0.1:{provider_port}/v1/chat/completions",
                    direct_payload,
                    samples,
                    warmup,
                    {"Authorization": "Bearer fixture-secret"},
                )
                routed = timed_requests(
                    f"http://127.0.0.1:{gateway_port}/v1/chat/completions",
                    gateway_payload,
                    samples,
                    warmup,
                )
                memory = resident_memory_mib(gateway.pid)
        except Exception:
            if gateway_log_path.exists():
                log = gateway_log_path.read_text(encoding="utf-8", errors="replace")
                if log:
                    print(log, file=sys.stderr)
            raise
        finally:
            stop_process(gateway)
            stop_process(provider)

    direct_p50 = percentile(direct, 50)
    gateway_p50 = percentile(routed, 50)
    return {
        "startup_ready_ms": startup_ms,
        "provider_direct_p50_ms": direct_p50,
        "provider_direct_p95_ms": percentile(direct, 95),
        "gateway_request_p50_ms": gateway_p50,
        "gateway_request_p95_ms": percentile(routed, 95),
        "provider_routing_overhead_p50_ms": max(0.0, gateway_p50 - direct_p50),
        "resident_memory_mib": memory,
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check", action="store_true", help="fail when a threshold is exceeded"
    )
    parser.add_argument(
        "--output", type=Path, help="write the full JSON result to this path"
    )
    parser.add_argument("--thresholds", type=Path, default=DEFAULT_THRESHOLDS)
    parser.add_argument("--samples", type=int, default=100)
    parser.add_argument("--warmup", type=int, default=10)
    parser.add_argument(
        "--binary",
        type=Path,
        help="measure an existing release binary and omit cold build metrics",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.samples < 1 or args.warmup < 0:
        raise SystemExit("--samples must be positive and --warmup must be non-negative")
    thresholds = json.loads(args.thresholds.read_text(encoding="utf-8"))
    build_dir: tempfile.TemporaryDirectory[str] | None = None
    try:
        if args.binary:
            binary = args.binary.resolve()
            debug_seconds = None
            release_seconds = None
        else:
            build_dir = tempfile.TemporaryDirectory(prefix="model-gateway-perf-build-")
            target_dir = Path(build_dir.name)
            debug_seconds, _ = build_binary(target_dir, release=False)
            release_seconds, binary = build_binary(target_dir, release=True)
        if not binary.is_file():
            raise SystemExit(f"release binary does not exist: {binary}")

        metrics: dict[str, float | None] = {
            "debug_build_seconds": debug_seconds,
            "release_build_seconds": release_seconds,
            **runtime_metrics(binary, args.samples, args.warmup),
            "release_binary_mib": binary.stat().st_size / (1024 * 1024),
        }
        failures = threshold_failures(metrics, thresholds) if args.check else []
        result = {
            "schema_version": 1,
            "recorded_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "git_sha": command_output("git", "rev-parse", "HEAD"),
            "rustc": command_output("rustc", "--version"),
            "platform": {"system": platform.system(), "machine": platform.machine()},
            "samples": args.samples,
            "warmup": args.warmup,
            "metrics": {
                name: None if value is None else round(value, 3)
                for name, value in metrics.items()
            },
            "thresholds": thresholds,
            "thresholds_checked": args.check,
            "passed": None if not args.check else not failures,
            "failures": failures,
        }
        rendered = json.dumps(result, indent=2, sort_keys=True)
        print(rendered)
        if args.output:
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(rendered + "\n", encoding="utf-8")
        if failures:
            for failure in failures:
                print(f"performance threshold failed: {failure}", file=sys.stderr)
            return 1
        return 0
    finally:
        if build_dir is not None:
            build_dir.cleanup()


if __name__ == "__main__":
    raise SystemExit(main())
