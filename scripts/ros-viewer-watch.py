#!/usr/bin/env python3
"""Keep an automatically selected viewer attached to the current simulator."""

import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time


def simulators():
    result = subprocess.run(
        ["docker", "ps", "--no-trunc", "--format", "{{.ID}}\t{{.Names}}\t{{.Image}}"],
        check=True, capture_output=True, text=True,
    )
    return [
        (container_id, name)
        for container_id, name, image in (line.split("\t") for line in result.stdout.splitlines())
        if image.startswith("gazebo-web-sim:")
    ]


def stop_viewer(process, cidfile):
    # Stop only the container created by this launch, even during a cargo build.
    if cidfile.exists():
        container_id = cidfile.read_text().strip()
        if len(container_id) == 64 and all(c in "0123456789abcdef" for c in container_id):
            try:
                subprocess.run(["docker", "stop", "--timeout", "3", container_id],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10)
            except (OSError, subprocess.TimeoutExpired):
                pass # Still terminate the docker client if the daemon is unavailable.
    if process.poll() is None:
        process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()


def supervise(launcher, viewer_args, shutdown, poll_interval=2):
    status = None
    while not shutdown():
        available = simulators()
        if len(available) != 1:
            message = "Waiting for a simulator…" if not available else (
                "Several simulators are running; pass --container NAME:\n  "
                + "\n  ".join(name for _, name in available)
            )
            if message != status:
                print(message, file=sys.stderr, flush=True)
                status = message
            if len(available) > 1:
                return 2
            time.sleep(poll_interval)
            continue

        selected_id, selected_name = available[0]
        print(f"Using simulator: {selected_name}", file=sys.stderr, flush=True)
        with tempfile.TemporaryDirectory(prefix="reflecdith-viewer-") as directory:
            cidfile = Path(directory) / "container.cid"
            environment = dict(os.environ, REFLECDITH_VIEWER_CIDFILE=str(cidfile))
            process = subprocess.Popen(
                [str(launcher), "--container", selected_name, "--", *viewer_args], env=environment,
            )
            disconnected = False
            try:
                while process.poll() is None and not shutdown():
                    if not any(cid == selected_id for cid, _ in simulators()):
                        print("Simulator was replaced or stopped; reconnecting…", file=sys.stderr, flush=True)
                        disconnected = True
                        break
                    time.sleep(poll_interval)
                if not disconnected and not shutdown():
                    # Closing the window (or a viewer/build failure) ends the launcher.
                    # It must not reopen a window the user deliberately closed.
                    return process.wait()
            finally:
                stop_viewer(process, cidfile)
        status = None
    return 0


def main():
    received_signal = 0

    def stop(signum, _frame):
        nonlocal received_signal
        received_signal = signum

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    try:
        result = supervise(Path(__file__).with_name("ros-viewer"), sys.argv[1:], lambda: received_signal)
        return 128 + received_signal if received_signal else result
    except (OSError, subprocess.SubprocessError) as error:
        print(f"Viewer launcher: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
