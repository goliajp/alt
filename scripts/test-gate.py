#!/usr/bin/env python3
"""Exercise the gate's command boundary without compiling the workspace."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


GATE = Path(__file__).resolve().with_name("gate.sh")
FAKE_CARGO = r'''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
args = sys.argv[1:]
with open(os.environ["GATE_CALLS"], "a") as log:
    log.write(json.dumps({"args": args, "cwd": os.getcwd(),
                          "corpus": os.environ.get("ALT_CORPUS")}) + "\n")
if args[0] == os.environ.get("GATE_FAIL"):
    sys.exit(17)
if args[:3] == ["tree", "-i", "ureq"]:
    print(f"alt-wire-http v0.0.0 ({os.environ['PWD']}/crates/alt-wire-http)")
    owner = os.environ.get("GATE_HTTP_OWNER")
    if owner:
        print(f"{owner} v0.0.0 ({os.environ['PWD']}/crates/{owner})")
if args[:3] == ["tree", "-i", "async-openai"]:
    owner = os.environ.get("GATE_LLM_OWNER")
    if owner:
        print(f"{owner} v0.0.0 ({os.environ['PWD']}/crates/{owner})")
'''


class GateTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="alt gate ")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        (self.root / "scripts").mkdir()
        self.gate = self.root / "scripts/gate.sh"
        shutil.copyfile(GATE, self.gate)
        (self.root / "scripts/test-package.py").write_text(
            "from pathlib import Path\nimport os,sys\n"
            "Path('package-called').write_text('yes')\n"
            "sys.exit(int(os.environ.get('GATE_PACKAGE_EXIT', '0')))\n"
        )
        for name in ("crates", "vendor", "fuzz", ".dev/corpus", "bin"):
            (self.root / name).mkdir(parents=True)
        cargo = self.root / "bin/cargo"
        cargo.write_text(FAKE_CARGO)
        cargo.chmod(0o755)
        self.log = self.root / "calls.jsonl"
        self.env = dict(os.environ, PATH=f"{cargo.parent}:{os.environ['PATH']}",
                        GATE_CALLS=str(self.log))
        for name in ("ALT_CORPUS", "GATE_FAIL", "GATE_PACKAGE_EXIT", "GATE_HTTP_OWNER", "GATE_LLM_OWNER"):
            self.env.pop(name, None)

    def run_gate(self, *args):
        self.log.unlink(missing_ok=True)
        result = subprocess.run(["sh", str(self.gate), *args], cwd="/",
                                env=self.env, text=True, capture_output=True)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()] \
            if self.log.exists() else []
        return result, calls

    def test_package_class_runs_without_cargo_and_rejects_scope(self):
        result, calls = self.run_gate("package")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls, [])
        self.assertTrue((self.root / "package-called").exists())
        for options in (["-p", "alt-cli"], ["--workspace"], ["--release"]):
            result, calls = self.run_gate("package", *options)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(calls, [])

    def test_package_failure_stops_full_gate_before_integration(self):
        self.env["GATE_PACKAGE_EXIT"] = "19"
        result, calls = self.run_gate("all", "-p", "alt-cli")
        self.assertEqual(result.returncode, 19)
        self.assertEqual([c["args"] for c in calls if c["args"][0] == "test"],
                         [["test", "--package", "alt-cli", "--lib", "--"]])
        self.assertNotIn(": OK", result.stdout)

    def test_scope_and_filter(self):
        result, calls = self.run_gate("it", "-p", "alt-cli", "--test", "it",
                                      "--filter", "workspace::undo", "--exact",
                                      "--release")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([c["args"] for c in calls], [[
            "test", "--package", "alt-cli", "--test", "it", "--release",
            "--", "--exact", "workspace::undo"]])
        self.assertEqual(calls[0]["cwd"], str(self.root.resolve()))

    def test_unit_scope(self):
        result, calls = self.run_gate("unit", "--package", "alt-refs",
                                      "--filter", "transaction")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls[0]["args"], ["test", "--package", "alt-refs",
                                         "--lib", "--", "transaction"])

    def test_filters_are_literal_arguments(self):
        literal = 'a b; $(touch injected) `touch injected` *'
        result, calls = self.run_gate("it", "-p", "alt-cli", "--filter", literal)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls[0]["args"][-1], literal)
        self.assertFalse((self.root / "injected").exists())

    def test_lint_scope_keeps_global_policy(self):
        result, calls = self.run_gate("lint", "-p", "alt-cli", "--release")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([c["args"] for c in calls[:2]], [
            ["fmt", "--package", "alt-cli", "--check"],
            ["clippy", "--package", "alt-cli", "--all-targets", "--release",
             "--", "-D", "warnings"]])
        self.assertTrue(all(c["args"][0] == "tree" for c in calls[2:]))
        self.assertGreater(len(calls), 2)

    def test_isolation_scan_errors_fail_the_gate(self):
        for directory in ("crates", "vendor", "fuzz"):
            with self.subTest(directory=directory):
                (self.root / directory).rmdir()
                try:
                    result, _ = self.run_gate("lint", "-p", "alt-cli")
                    self.assertNotEqual(result.returncode, 0, result.stderr)
                    self.assertNotIn(": OK", result.stdout)
                finally:
                    (self.root / directory).mkdir()

    def test_isolation_scan_rejects_forbidden_source_in_each_tree(self):
        for directory in ("crates", "vendor", "fuzz"):
            with self.subTest(directory=directory):
                source = self.root / directory / "forbidden.rs"
                source.write_text("use openai::Client;\n")
                try:
                    result, _ = self.run_gate("lint", "-p", "alt-cli")
                    self.assertNotEqual(result.returncode, 0, result.stderr)
                    self.assertIn("LLM reference in core source", result.stderr)
                    self.assertNotIn(": OK", result.stdout)
                finally:
                    source.unlink()

    def test_isolation_scan_execution_error_is_not_a_clean_result(self):
        real_grep = shutil.which("grep")
        self.assertIsNotNone(real_grep)
        grep = self.root / "bin/grep"
        grep.write_text(
            "#!/usr/bin/env python3\nimport os,sys\n"
            "if sys.argv[1:2] == ['-rniE']: sys.exit(2)\n"
            f"os.execv({real_grep!r}, [{real_grep!r}, *sys.argv[1:]])\n"
        )
        grep.chmod(0o755)
        result, _ = self.run_gate("lint", "-p", "alt-cli")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("source scan failed (exit 2)", result.stderr)
        self.assertNotIn(": OK", result.stdout)

    def test_server_transport_is_allowed_but_core_http_and_server_llm_are_denied(self):
        self.env["GATE_HTTP_OWNER"] = "alt-server"
        result, _ = self.run_gate("lint", "-p", "alt-server")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.env["GATE_HTTP_OWNER"] = "alt-cloud-client"
        result, _ = self.run_gate("lint", "-p", "alt-cloud-client")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.env["GATE_HTTP_OWNER"] = "golia-platform-contract"
        result, _ = self.run_gate("lint", "-p", "alt-cloud-client")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("golia-platform-contract depends on HTTP", result.stderr)
        self.env["GATE_HTTP_OWNER"] = "alt-server-store"
        result, _ = self.run_gate("lint", "-p", "alt-server")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("alt-server-store depends on HTTP", result.stderr)
        self.env["GATE_HTTP_OWNER"] = "alt-server"
        self.env["GATE_LLM_OWNER"] = "alt-server"
        result, _ = self.run_gate("lint", "-p", "alt-server")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("alt-server pulls in", result.stderr)

    def test_all_default_and_explicit_scopes(self):
        for options, scope, fmt_scope in [
            ([], ["--workspace"], ["--all"]),
            (["--workspace"], ["--workspace"], ["--all"]),
            (["-p", "alt-cli"], ["--package", "alt-cli"], ["--package", "alt-cli"]),
        ]:
            with self.subTest(options=options):
                result, calls = self.run_gate("all", *options)
                self.assertEqual(result.returncode, 0, result.stderr)
                commands = [c["args"] for c in calls if c["args"][0] != "tree"]
                self.assertEqual(commands, [
                    ["fmt", *fmt_scope, "--check"],
                    ["clippy", *scope, "--all-targets", "--", "-D", "warnings"],
                    ["test", *scope, "--lib", "--"],
                    ["test", *scope, "--tests", "--"],
                    ["test", *scope, "--tests", "--", "--ignored"]])
                self.assertEqual(calls[-1]["corpus"], str(self.root.resolve() / ".dev/corpus"))

    def test_relative_corpus_override(self):
        (self.root / "custom corpus").mkdir()
        self.env["ALT_CORPUS"] = "custom corpus"
        result, calls = self.run_gate("corpus", "-p", "alt-cli", "--test", "it")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls[0]["corpus"], str(self.root.resolve() / "custom corpus"))

    def test_corpus_filter_covers_ignored_library_tests_without_target(self):
        result, calls = self.run_gate("corpus", "-p", "alt-server-store",
                                      "--filter", "catalog::blocks::", "--release")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([c["args"] for c in calls], [
            ["test", "--package", "alt-server-store", "--tests", "--release",
             "--", "--ignored", "catalog::blocks::"],
        ])

    def test_missing_corpus_fails_before_cargo(self):
        self.env["ALT_CORPUS"] = "missing corpus"
        result, calls = self.run_gate("corpus", "-p", "alt-cli")
        self.assertEqual(result.returncode, 2)
        self.assertEqual(calls, [])

    def test_invalid_arguments_never_run_cargo(self):
        cases = [[], ["unknown"], ["unit", "--pakage", "alt-cli"],
                 ["unit", "alt-cli"], ["unit", "--"], ["unit", "--release=yes"],
                 ["unit", "--package", "alt-*"], ["unit", "--exact"],
                 ["unit", "--filter", "x"], ["it", "--test", "it"],
                 ["unit", "-p", "a", "--test", "it"],
                 ["all", "-p", "a", "--filter", "x"],
                 ["lint", "-p", "a", "--filter", "x"],
                 ["unit", "-p", "a", "--workspace"],
                 ["unit", "--workspace", "-p", "a"],
                 ["unit", "--workspace", "--workspace"],
                 ["unit", "-p", "a", "-p", "b"],
                 ["unit", "--release", "--release"],
                 ["unit", "-p", "a", "--filter", "x", "--filter", "y"],
                 ["it", "-p", "a", "--test", "it", "--test", "it"],
                 ["unit", "-p", "a", "--filter", "x", "--exact", "--exact"]]
        for option in ("-p", "--package", "--test", "--filter"):
            for tail in ([], [""], ["--release"]):
                cases.append(["it", option, *tail])
        for args in cases:
            with self.subTest(args=args):
                result, calls = self.run_gate(*args)
                self.assertEqual(result.returncode, 2, result.stdout)
                self.assertEqual(calls, [])
                self.assertNotIn(": OK", result.stdout)

    def test_failure_stops_all(self):
        for command in ("fmt", "clippy", "test", "tree"):
            with self.subTest(command=command):
                self.env["GATE_FAIL"] = command
                result, calls = self.run_gate("all", "-p", "alt-cli")
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn(": OK", result.stdout)
                self.assertEqual(calls[-1]["args"][0], command)
                if command != "tree":
                    self.assertEqual(result.returncode, 17)

    def test_help_does_not_run_cargo(self):
        for args in (["--help"], ["unit", "--help"]):
            result, calls = self.run_gate(*args)
            self.assertEqual(result.returncode, 0)
            self.assertIn("Usage:", result.stdout)
            self.assertEqual(calls, [])


if __name__ == "__main__":
    unittest.main()
