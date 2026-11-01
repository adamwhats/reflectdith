import importlib.util
from pathlib import Path
import unittest
from unittest.mock import Mock, patch
import tempfile

spec = importlib.util.spec_from_file_location("viewer_watch", Path(__file__).with_name("ros-viewer-watch.py"))
watch = importlib.util.module_from_spec(spec)
spec.loader.exec_module(watch)


class Process:
    def __init__(self, code=None):
        self.code = code

    def poll(self):
        return self.code

    def wait(self, **_):
        return self.code

    def terminate(self):
        self.code = -15


class WatchTests(unittest.TestCase):
    def run_watch(self, graphs, processes):
        actions = []

        def launch(command, **_):
            actions.append(("launch", command))
            return processes.pop(0)

        def stop(process, _):
            actions.append(("stop", process))
            if process.poll() is None:
                process.terminate()

        with patch.object(watch, "simulators", side_effect=graphs), \
             patch.object(watch.subprocess, "Popen", side_effect=launch), \
             patch.object(watch, "stop_viewer", side_effect=stop), \
             patch.object(watch.time, "sleep"):
            code = watch.supervise(Path("/launcher"), ["--hide-gui"], lambda: False)
        return code, actions

    def test_replaces_stale_network_before_launching_new_simulator(self):
        old, new = Process(), Process(0)
        code, actions = self.run_watch(
            [[("old-id", "old")], [("new-id", "new")], [("new-id", "new")]], [old, new],
        )
        self.assertEqual(code, 0)
        self.assertEqual([kind for kind, _ in actions], ["launch", "stop", "launch", "stop"])
        self.assertEqual(actions[0][1], ["/launcher", "--container", "old", "--", "--hide-gui"])
        self.assertEqual(actions[2][1], ["/launcher", "--container", "new", "--", "--hide-gui"])
        self.assertEqual(old.code, -15)
        self.assertEqual(new.code, 0)

    def test_recreated_container_with_same_name_has_a_new_network(self):
        code, actions = self.run_watch(
            [[("first-id", "sim")], [("second-id", "sim")], [("second-id", "sim")]],
            [Process(), Process(0)],
        )
        self.assertEqual(code, 0)
        self.assertEqual(sum(kind == "launch" for kind, _ in actions), 2)

    def test_window_close_and_crash_do_not_reopen_the_viewer(self):
        for exit_code in [0, 139]:
            with self.subTest(exit_code=exit_code):
                code, actions = self.run_watch([[("id", "sim")]], [Process(exit_code)])
                self.assertEqual(code, exit_code)
                self.assertEqual(sum(kind == "launch" for kind, _ in actions), 1)

    def test_waits_for_a_simulator_to_appear(self):
        code, actions = self.run_watch([[], [("id", "sim")]], [Process(0)])
        self.assertEqual(code, 0)
        self.assertEqual(sum(kind == "launch" for kind, _ in actions), 1)

    def test_cleanup_stops_only_the_viewer_container_recorded_by_this_launch(self):
        viewer_id = "a" * 64
        process = Process()
        with tempfile.TemporaryDirectory() as directory:
            cidfile = Path(directory) / "viewer.cid"
            cidfile.write_text(viewer_id)
            with patch.object(watch.subprocess, "run") as docker:
                watch.stop_viewer(process, cidfile)
            self.assertEqual(docker.call_args.args[0], ["docker", "stop", "--timeout", "3", viewer_id])
            self.assertEqual(process.code, -15)

    def test_shutdown_stops_the_active_viewer_without_restarting(self):
        process = Process()
        with patch.object(watch, "simulators", return_value=[("id", "sim")]), \
             patch.object(watch.subprocess, "Popen", return_value=process) as launch, \
             patch.object(watch, "stop_viewer") as stop:
            shutdown = Mock(side_effect=[False, True, True, True])
            self.assertEqual(watch.supervise(Path("/launcher"), [], shutdown), 0)
            launch.assert_called_once()
            stop.assert_called_once()

    def test_ambiguous_simulators_require_an_explicit_choice(self):
        code, actions = self.run_watch([[("one", "sim-one"), ("two", "sim-two")]], [])
        self.assertEqual(code, 2)
        self.assertEqual(actions, [])


if __name__ == "__main__":
    unittest.main()
