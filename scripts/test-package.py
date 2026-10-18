#!/usr/bin/env python3
"""Installer boundary tests; real native packages are validated separately."""

import hashlib
import json
from pathlib import Path
import platform
import runpy
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch


SOURCE = Path(__file__).with_name("install-internal.py").resolve()
M = runpy.run_path(str(SOURCE))


@unittest.skipUnless((platform.system(), platform.machine()) in M["TARGETS"],
                     "internal installer supports macOS arm64 and Linux x86_64")
class PackageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="alt-package-test-")
        self.root = Path(self.temp.name)
        self.prefix = self.root / "installed"
        self.package = self.make_package("one", "0.1.0-internal.1")

    def tearDown(self):
        self.temp.cleanup()

    def make_package(self, name, version):
        root = self.root / name
        root.mkdir()
        rows = {}
        for relative in M["FILES"]:
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            content = (f"#!/bin/sh\nprintf '%s\\n' '{version}'\n".encode()
                       if relative.startswith("bin/") else b"test payload\n")
            if relative == "install.py":
                content = SOURCE.read_bytes()
            elif relative == "examples/task-workflow.py":
                content = (SOURCE.parent.parent / relative).read_bytes()
            path.write_bytes(content)
            mode = 0o755 if relative.startswith("bin/") else 0o644
            path.chmod(mode)
            rows[relative] = {"sha256": hashlib.sha256(content).hexdigest(), "bytes": len(content), "mode": mode}
        manifest = {"format": M["FORMAT"], "version": version, "files": rows,
                    "target": M["TARGETS"][(platform.system(), platform.machine())]}
        (root / "manifest.json").write_text(json.dumps(manifest, sort_keys=True))
        return root

    def test_install_upgrade_select_and_uninstall_preserve_unrelated_data(self):
        other = self.root / "work.txt"
        other.write_text("keep")
        first = M["install"](self.package, self.prefix)
        again = M["install"](self.package, self.prefix)
        self.assertEqual(first, again)
        second_package = self.make_package("two", "0.1.0-internal.2")
        second = M["install"](second_package, self.prefix)
        self.assertEqual(len(second["releases"]), 2)
        self.assertNotEqual(first["active"], second["active"])
        self.assertEqual(subprocess.check_output([self.prefix / "bin/alt"]).strip(), b"0.1.0-internal.2")
        M["manage"](self.prefix, "activate", first["active"])
        self.assertEqual(subprocess.check_output([self.prefix / "bin/alt"]).strip(), b"0.1.0-internal.1")
        M["manage"](self.prefix, "uninstall")
        self.assertFalse(self.prefix.exists())
        self.assertEqual(other.read_text(), "keep")

    def test_modified_new_package_does_not_change_active_release(self):
        state = M["install"](self.package, self.prefix)
        (self.package / "bin/alt").write_bytes(b"changed")
        with self.assertRaises(ValueError):
            M["install"](self.package, self.prefix)
        self.assertEqual(M["manage"](self.prefix, "status")["active"], state["active"])

    def test_unknown_or_linked_payload_and_inventory_traversal_are_refused(self):
        for kind in ["extra", "symlink", "traversal", "wrong-target"]:
            with self.subTest(kind=kind):
                package = self.make_package(kind, "0.1.0-internal.1")
                if kind == "extra":
                    (package / "private.txt").write_text("do not install")
                elif kind == "symlink":
                    (package / "README.md").unlink()
                    (package / "README.md").symlink_to(self.package / "README.md")
                else:
                    path = package / "manifest.json"
                    data = json.loads(path.read_text())
                    if kind == "traversal":
                        data["files"]["../../outside"] = data["files"].pop("README.md")
                    else:
                        data["target"] = "unsupported-target"
                    path.write_text(json.dumps(data))
                with self.assertRaises(ValueError):
                    M["install"](package, self.prefix)
                self.assertFalse(self.prefix.exists())

    def test_foreign_prefix_and_unknown_installed_files_are_preserved(self):
        self.prefix.mkdir()
        foreign = self.prefix / "other.txt"
        foreign.write_text("keep")
        with self.assertRaises(FileNotFoundError):
            M["install"](self.package, self.prefix)
        self.assertEqual(foreign.read_text(), "keep")
        foreign.unlink()
        self.prefix.rmdir()
        M["install"](self.package, self.prefix)
        foreign.write_text("still keep")
        with self.assertRaises(ValueError):
            M["manage"](self.prefix, "uninstall")
        self.assertEqual(foreign.read_text(), "still keep")

    def test_tampered_old_release_and_retargeted_links_are_not_removed(self):
        state = M["install"](self.package, self.prefix)
        payload = self.prefix / "releases" / state["active"] / "README.md"
        payload.write_text("local edit")
        with self.assertRaises(ValueError):
            M["manage"](self.prefix, "uninstall")
        self.assertEqual(payload.read_text(), "local edit")
        (self.prefix / "current").unlink()
        (self.prefix / "current").symlink_to(self.root)
        with self.assertRaises(ValueError):
            M["manage"](self.prefix, "uninstall")
        self.assertTrue(self.root.exists())

    def test_switch_error_and_busy_lock_preserve_current(self):
        state = M["install"](self.package, self.prefix)
        with patch("os.replace", side_effect=OSError("simulated publication error")):
            with self.assertRaises(OSError):
                M["manage"](self.prefix, "activate", state["active"])
        self.assertEqual(M["manage"](self.prefix, "status")["active"], state["active"])
        with M["lock_prefix"](self.prefix):
            with self.assertRaises(BlockingIOError):
                M["manage"](self.prefix, "uninstall")
        self.assertTrue((self.prefix / "bin/alt").is_file())

    def test_repository_prefix_refused_and_cli_errors_are_structured(self):
        repo = self.root / "repo"
        repo.mkdir()
        (repo / ".alt").mkdir()
        with self.assertRaises(ValueError):
            M["prefix_path"](str(repo / "tools"))
        (repo / "nested").mkdir()
        alias = self.root / "alias"
        alias.symlink_to(repo / "nested", target_is_directory=True)
        with self.assertRaises(ValueError):
            M["prefix_path"](str(alias / "tools"))
        example = subprocess.run([sys.executable, self.package / "examples/task-workflow.py",
                                  "--alt", self.package / "bin/alt", "--git", self.package / "bin/alt",
                                  "--directory", alias / "example"], capture_output=True)
        self.assertEqual(example.returncode, 2)
        self.assertFalse((repo / "nested/example").exists())
        result = subprocess.run([sys.executable, self.package / "install.py", "install", "--prefix", repo / "tools"], capture_output=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn("error", json.loads(result.stderr))
        self.assertFalse(result.stdout)

    def test_sigkill_before_and_after_current_switch_leaves_a_complete_release(self):
        first = M["install"](self.package, self.prefix)
        second = M["install"](self.make_package("two", "0.1.0-internal.2"), self.prefix)
        for boundary in ["before", "after"]:
            M["manage"](self.prefix, "activate", first["active"])
            ready = self.root / f"ready-{boundary}"
            code = '''import os,runpy,signal,sys
from pathlib import Path
m=runpy.run_path(sys.argv[1]); original=os.replace
def stopped():
    Path(sys.argv[5]).write_text("ready")
    signal.pause()
def replace(a,b):
    if sys.argv[4]=="before": stopped()
    original(a,b)
    if sys.argv[4]=="after": stopped()
os.replace=replace
m["manage"](Path(sys.argv[2]),"activate",sys.argv[3])
'''
            child = subprocess.Popen([sys.executable, "-c", code, SOURCE, self.prefix, second["active"], boundary, ready])
            try:
                deadline = time.monotonic() + 10
                while not ready.exists() and child.poll() is None and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertTrue(ready.exists(), "controlled publication boundary was not reached")
                child.kill()  # Only the child PID created above; it spawns no children.
                self.assertEqual(child.wait(timeout=5), -signal.SIGKILL)
            finally:
                if child.poll() is None:
                    child.kill()
                    child.wait(timeout=5)
            wanted = first if boundary == "before" else second
            self.assertEqual(M["manage"](self.prefix, "status")["active"], wanted["active"])


if __name__ == "__main__":
    unittest.main()
