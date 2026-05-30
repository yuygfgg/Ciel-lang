#!/usr/bin/env python3
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time


def fail(message):
    raise AssertionError(message)


class EchoServer:
    def __init__(self, port=0):
        self._stop = threading.Event()
        self._threads = []
        self._sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._sock.bind(("127.0.0.1", port))
        self._sock.listen()
        self._sock.settimeout(0.1)
        self.port = self._sock.getsockname()[1]
        self._thread = threading.Thread(target=self._accept_loop, daemon=True)
        self._thread.start()

    def _accept_loop(self):
        while not self._stop.is_set():
            try:
                conn, _ = self._sock.accept()
            except socket.timeout:
                continue
            except OSError:
                return
            thread = threading.Thread(target=self._serve_client, args=(conn,), daemon=True)
            self._threads.append(thread)
            thread.start()

    def _serve_client(self, conn):
        with conn:
            conn.settimeout(5.0)
            while True:
                try:
                    data = conn.recv(32768)
                except OSError:
                    return
                if not data:
                    return
                try:
                    conn.sendall(data)
                except OSError:
                    return

    def stop(self):
        self._stop.set()
        try:
            self._sock.close()
        except OSError:
            pass
        self._thread.join(timeout=1.0)


def free_port():
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def wait_for_log(path, needle, proc, timeout=8.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            fail(f"process exited before log marker {needle!r}")
        try:
            with open(path, "r", encoding="utf-8", errors="replace") as handle:
                if needle in handle.read():
                    return
        except FileNotFoundError:
            pass
        time.sleep(0.05)
    fail(f"timed out waiting for log marker {needle!r}")


def wait_exit(proc, timeout=8.0):
    try:
        return proc.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        fail("process did not exit in time")


def start_process(args, log_path):
    log = open(log_path, "ab", buffering=0)
    proc = subprocess.Popen(args, stdout=log, stderr=subprocess.STDOUT)
    proc._ciel_log = log
    return proc


def stop_process(proc):
    if proc is None:
        return
    if proc.poll() is None:
        proc.terminate()
        try:
            proc.wait(timeout=2.0)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=2.0)
    log = getattr(proc, "_ciel_log", None)
    if log is not None:
        log.close()


def tunnel_request(public_port, payload, timeout=8.0):
    with socket.create_connection(("127.0.0.1", public_port), timeout=timeout) as sock:
        sock.settimeout(timeout)
        sock.sendall(payload)
        sock.shutdown(socket.SHUT_WR)
        chunks = []
        while True:
            data = sock.recv(65536)
            if not data:
                break
            chunks.append(data)
        return b"".join(chunks)


def expect_echo(public_port, payload):
    got = tunnel_request(public_port, payload)
    if got != payload:
        fail(f"echo mismatch: expected {len(payload)} bytes, got {len(got)} bytes")


def expect_closed(public_port):
    got = tunnel_request(public_port, b"target unavailable")
    if got != b"":
        fail(f"expected closed stream, got {len(got)} bytes")


def run_concurrent(public_port):
    payloads = [
        b"left-" + (b"a" * 8192),
        b"right-" + (b"b" * 12288),
    ]
    errors = []

    def worker(payload):
        try:
            expect_echo(public_port, payload)
        except Exception as error:
            errors.append(error)

    threads = [threading.Thread(target=worker, args=(payload,)) for payload in payloads]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join(timeout=12.0)
    for thread in threads:
        if thread.is_alive():
            fail("concurrent client did not finish")
    if errors:
        raise errors[0]


def dump_logs(paths):
    for label, path in paths:
        print(f"--- {label} log ---", file=sys.stderr)
        try:
            with open(path, "r", encoding="utf-8", errors="replace") as handle:
                sys.stderr.write(handle.read())
        except FileNotFoundError:
            print("(missing)", file=sys.stderr)


def main():
    if len(sys.argv) != 3:
        print("usage: integration.py SERVER_EXE AGENT_EXE", file=sys.stderr)
        return 2
    server_exe = os.path.abspath(sys.argv[1])
    agent_exe = os.path.abspath(sys.argv[2])

    with tempfile.TemporaryDirectory(prefix="ciel_tunnel_integration_") as tmp:
        control_port = free_port()
        public_port = free_port()
        echo = EchoServer()
        target_port = echo.port

        server_log = os.path.join(tmp, "server.log")
        wrong_agent_log = os.path.join(tmp, "wrong-agent.log")
        agent_log = os.path.join(tmp, "agent.log")
        logs = [
            ("server", server_log),
            ("wrong-agent", wrong_agent_log),
            ("agent", agent_log),
        ]

        server = None
        agent = None
        try:
            server = start_process([
                server_exe,
                "--control", f"127.0.0.1:{control_port}",
                "--public", f"127.0.0.1:{public_port}",
                "--route", "dev",
                "--psk", "secret-tunnel-key",
            ], server_log)
            wait_for_log(server_log, "tunnel-server: ready", server)

            wrong_agent = start_process([
                agent_exe,
                "--server", f"127.0.0.1:{control_port}",
                "--target", f"127.0.0.1:{target_port}",
                "--route", "dev",
                "--psk", "wrong-tunnel-key",
            ], wrong_agent_log)
            if wait_exit(wrong_agent) == 0:
                fail("wrong-psk agent unexpectedly succeeded")
            stop_process(wrong_agent)

            agent = start_process([
                agent_exe,
                "--server", f"127.0.0.1:{control_port}",
                "--target", f"127.0.0.1:{target_port}",
                "--route", "dev",
                "--psk", "secret-tunnel-key",
            ], agent_log)
            wait_for_log(agent_log, "tunnel-agent: authenticated", agent)

            expect_echo(public_port, b"first sequential payload")
            expect_echo(public_port, b"second sequential payload")
            run_concurrent(public_port)
            expect_echo(public_port, b"large-" + (b"x" * 70000))

            with socket.create_connection(("127.0.0.1", public_port), timeout=5.0) as sock:
                sock.sendall(b"client closes early")

            expect_echo(public_port, b"after early close")

            echo.stop()
            expect_closed(public_port)
            echo = EchoServer(target_port)
            time.sleep(0.1)
            expect_echo(public_port, b"after target recovery")
        except Exception:
            dump_logs(logs)
            raise
        finally:
            echo.stop()
            stop_process(agent)
            stop_process(server)
    return 0


if __name__ == "__main__":
    sys.exit(main())
