"""Preparation checks for the separate native-tree experiment; no model or server."""

import copy
import importlib.util
import pathlib
import subprocess
import sys
import tempfile
import tomllib
import unittest


sys.dont_write_bytecode = True
ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "experiments/codex_tree.py"
spec = importlib.util.spec_from_file_location("codex_tree", SCRIPT)
experiment = importlib.util.module_from_spec(spec)
spec.loader.exec_module(experiment)


class Preparation(unittest.TestCase):
    def setUp(self):
        with (ROOT / "tests/fixtures/crew-codex-tree.toml").open("rb") as file:
            self.source = tomllib.load(file)

    def test_only_coordinator_is_a_herdr_session_and_source_is_preserved(self):
        self.source["common_prompt"] += " Use only Claude SendMessage."
        original = copy.deepcopy(self.source)
        generated, roles = experiment.prepare(self.source, "/tmp/tree-sandbox")
        parsed = tomllib.loads(generated)
        self.assertTrue(parsed["experimental_codex_tree"])
        self.assertEqual([role["name"] for role in parsed["roles"]], ["tree-lead"])
        self.assertEqual([role["task_name"] for role in roles], ["tree_reviewer", "tree_coder"])
        prompt = parsed["roles"][0]["prompt"]
        self.assertIn("Only tree-lead writes .herdr/status.json", prompt)
        self.assertIn("replaces earlier SendMessage", prompt)
        self.assertIn("Use only Claude SendMessage", prompt)
        self.assertIn("If you are a child", prompt)
        self.assertNotIn("{{", prompt)
        self.assertEqual(parsed["codex"]["sandbox"], "workspace-write")
        self.assertEqual(original, self.source)

    def test_prepare_cli_writes_only_a_new_destination(self):
        source = ROOT / "tests/fixtures/crew-codex-tree.toml"
        original = source.read_bytes()
        with tempfile.TemporaryDirectory() as directory:
            process = subprocess.run([sys.executable, str(SCRIPT), "--source", str(source),
                                      "--root", directory], capture_output=True, text=True)
            self.assertEqual(process.returncode, 0, process.stderr)
            target = pathlib.Path(directory) / ".herdr/crew.toml"
            generated = tomllib.loads(target.read_text())
            self.assertEqual(len(generated["roles"]), 1)
        self.assertEqual(source.read_bytes(), original)

    def test_incompatible_native_task_names_are_rejected(self):
        self.source["roles"][2]["name"] = "tree_reviewer"
        with self.assertRaisesRegex(ValueError, "collide"):
            experiment.prepare(self.source, "/tmp/tree-sandbox")

    def test_independent_permissions_and_worktrees_are_not_silently_lost(self):
        for change in [{"kind": "claude"}, {"worktree": True},
                       {"codex": {"sandbox": "read-only"}}]:
            with self.subTest(change=change):
                source = copy.deepcopy(self.source)
                source["roles"][1].update(change)
                with self.assertRaises(ValueError):
                    experiment.prepare(source, "/tmp/tree-sandbox")

    def test_extras_and_initial_messages_are_rejected_instead_of_discarded(self):
        for change in [{"extra": True}, {"start_message": "Initialize"}, {"start_message": ""}]:
            for index in [0, 1]:
                with self.subTest(change=change, role=index):
                    source = copy.deepcopy(self.source)
                    source["roles"][index].update(change)
                    with self.assertRaisesRegex(ValueError, "unsupported"):
                        experiment.prepare(source, "/tmp/tree-sandbox")
        self.source["start_message"] = "Initialize all roles"
        with self.assertRaisesRegex(ValueError, "start_message is unsupported"):
            experiment.prepare(self.source, "/tmp/tree-sandbox")

    def test_preparation_never_overwrites_an_existing_crew(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / ".herdr").mkdir()
            source = root / ".herdr/crew.toml"
            source.write_bytes((ROOT / "tests/fixtures/crew-codex-tree.toml").read_bytes())
            original = source.read_bytes()
            process = subprocess.run([sys.executable, str(SCRIPT), "--source", str(source),
                                      "--root", str(root)], capture_output=True, text=True)
            self.assertNotEqual(process.returncode, 0)
            self.assertEqual(source.read_bytes(), original)

    def test_preparation_refuses_a_symlinked_destination(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "outside").mkdir()
            (root / ".herdr").symlink_to(root / "outside", target_is_directory=True)
            process = subprocess.run([sys.executable, str(SCRIPT), "--source",
                                      str(ROOT / "tests/fixtures/crew-codex-tree.toml"),
                                      "--root", str(root)], capture_output=True, text=True)
            self.assertNotEqual(process.returncode, 0)
            self.assertFalse((root / "outside/crew.toml").exists())


if __name__ == "__main__":
    unittest.main()
