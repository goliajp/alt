#!/usr/bin/env python3
"""The zsh plugin in the internal package: carried, installed, and named."""

import hashlib
import json
from pathlib import Path
import platform
import runpy
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent
M = runpy.run_path(str(ROOT / "scripts/install-internal.py"))
P = runpy.run_path(str(ROOT / "scripts/package-internal.py"))
ZSH = "share/alt/zsh"


class PayloadTests(unittest.TestCase):
    def test_every_file_of_the_plugin_is_packaged_from_the_repository(self):
        plugin = ROOT / "shell/zsh"
        held = {path.relative_to(plugin).as_posix() for path in plugin.rglob("*") if path.is_file()}
        packaged = {name.removeprefix(ZSH + "/"): source
                    for name, source in P["PAYLOAD"].items() if name.startswith(ZSH + "/")}
        self.assertEqual(set(packaged), held)
        for name, source in packaged.items():
            self.assertEqual(source, f"shell/zsh/{name}")
        self.assertEqual(P["PAYLOAD"]["docs/shell.md"], "docs/shell.md")
        # what the packager writes is what the installer takes
        self.assertEqual(set(P["PAYLOAD"]) | {"bin/alt", "bin/altd", "bin/altd-server"}, M["FILES"])


@unittest.skipUnless((platform.system(), platform.machine()) in M["TARGETS"],
                     "internal installer supports macOS arm64 and Linux x86_64")
class InstallTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="alt-package-shell-test-")
        self.root = Path(self.temp.name)
        self.prefix = self.root / "installed"

    def tearDown(self):
        self.temp.cleanup()

    def make_package(self, name, version, format):
        root = self.root / name
        rows = {}
        for relative in M["PAYLOAD_FORMATS"][format]:
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            source = P["PAYLOAD"].get(relative, "")
            content = (ROOT / source).read_bytes() if source.startswith("shell/") else b"test payload\n"
            path.write_bytes(content)
            mode = 0o755 if relative.startswith("bin/") else 0o644
            path.chmod(mode)
            rows[relative] = {"sha256": hashlib.sha256(content).hexdigest(), "bytes": len(content), "mode": mode}
        manifest = {"format": format, "version": version, "files": rows,
                    "target": M["TARGETS"][(platform.system(), platform.machine())]}
        (root / "manifest.json").write_text(json.dumps(manifest, sort_keys=True))
        return root

    def test_the_plugin_is_installed_and_the_line_for_zshrc_is_given(self):
        package = self.make_package("three", "0.1.0-internal.3", M["FORMAT"])
        result = M["install"](package, self.prefix)
        plugin = self.prefix / "current" / ZSH
        self.assertEqual(result["zsh"], f"source {plugin}/alt.plugin.zsh")
        for name in ["alt.plugin.zsh", "_alt", "functions/alt_prompt_info", "functions/+vi-alt-backend"]:
            self.assertEqual((plugin / name).read_bytes(), (ROOT / "shell/zsh" / name).read_bytes())
            self.assertEqual((plugin / name).stat().st_mode & 0o777, 0o644)
        self.assertTrue((self.prefix / "current/docs/shell.md").is_file())

    def test_a_package_from_before_the_plugin_installs_as_before(self):
        old = self.make_package("two", "0.1.0-internal.2", "alt-internal-package-v2")
        result = M["install"](old, self.prefix)
        self.assertNotIn("zsh", result)
        self.assertFalse((self.prefix / "current/share").exists())
        new = self.make_package("three", "0.1.0-internal.3", M["FORMAT"])
        both = M["install"](new, self.prefix)
        self.assertEqual(len(both["releases"]), 2)
        M["manage"](self.prefix, "activate", result["active"])
        self.assertFalse((self.prefix / "current/share").exists())

    def test_a_file_that_is_not_of_the_plugin_is_refused(self):
        package = self.make_package("three", "0.1.0-internal.3", M["FORMAT"])
        (package / ZSH / "functions/extra").write_text("print extra\n")
        with self.assertRaises(ValueError):
            M["install"](package, self.prefix)
        self.assertFalse(self.prefix.exists())
        old = self.make_package("two", "0.1.0-internal.2", "alt-internal-package-v2")
        (old / "share").mkdir()
        (old / "share/alt.plugin.zsh").write_text("print extra\n")
        with self.assertRaises(ValueError):
            M["install"](old, self.prefix)


if __name__ == "__main__":
    unittest.main()
