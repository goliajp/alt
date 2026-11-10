#!/usr/bin/env python3
"""Installer boundary tests; real native packages are validated separately."""

import copy
import hashlib
import io
import tarfile
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

    def make_package(self, name, version, format=None):
        root = self.root / name
        root.mkdir()
        rows = {}
        format = format or M["FORMAT"]
        for relative in M["PAYLOAD_FORMATS"][format]:
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
        manifest = {"format": format, "version": version, "files": rows,
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

    def test_legacy_and_current_formats_coexist_and_can_be_selected(self):
        legacy = self.make_package("legacy", "0.1.0-internal.1", "alt-internal-package-v1")
        before = M["install"](legacy, self.prefix)
        after = M["install"](self.package, self.prefix)
        self.assertEqual(len(after["releases"]), 2)
        self.assertNotEqual(before["active"], after["active"])
        self.assertTrue((self.prefix / "current/docs/change-links.md").is_file())
        M["manage"](self.prefix, "activate", before["active"])
        self.assertFalse((self.prefix / "current/docs/change-links.md").exists())
        self.assertEqual(M["manage"](self.prefix, "status")["active"], before["active"])
        M["manage"](self.prefix, "uninstall")
        self.assertFalse(self.prefix.exists())

    def test_new_payload_cannot_claim_the_legacy_format(self):
        path = self.package / "manifest.json"
        manifest = json.loads(path.read_text())
        manifest["format"] = "alt-internal-package-v1"
        path.write_text(json.dumps(manifest))
        with self.assertRaises(ValueError):
            M["install"](self.package, self.prefix)
        self.assertFalse(self.prefix.exists())

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


class ServerBundleTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="alt-server-package-test-")
        self.root = Path(self.temp.name)
        self.manager = runpy.run_path(str(SOURCE.parent.parent / "self-hosted/manage.py"))

    def tearDown(self):
        self.temp.cleanup()

    def make_bundle(self, version):
        root = self.root / str(version)
        root.mkdir()
        files = {"images.tar", "compose.yaml", "Caddyfile", "manage.py", "example-config.json",
                 "README.md", "dependencies.json", "LICENSE-MIT", "LICENSE-APACHE", "server-operator.md"}
        if version >= 2:
            files |= {"docs/task-remote.md", "docs/change-links.md"}
        if version >= 3:
            files.add("image_identity.py")
        hashes = {}
        for name in files:
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(name.encode())
            hashes[name] = hashlib.sha256(path.read_bytes()).hexdigest()
        ids = ["sha256:" + letter * 64 for letter in "abc"]
        images = [{"reference": "test/image-" + str(i) + ":one", "id": value,
                   **({"config_digest": value} if version >= 3 else {})}
                  for i, value in enumerate(ids)]
        manifest = {"format": version, "product": "alt-server", "files": hashes,
                    "architecture": "arm64", "images": images,
                    "application_image": ids[0], "database_image": ids[1], "edge_image": ids[2]}
        (root / "bundle.json").write_text(json.dumps(manifest))
        return root

    def test_all_formats_verify_and_new_document_tampering_is_refused(self):
        for version in [1, 2, 3]:
            root = self.make_bundle(version)
            self.assertEqual(self.manager["bundle"](root)["format"], version)
        root = self.root / "2"
        (root / "docs/change-links.md").write_text("modified")
        with self.assertRaisesRegex(ValueError, "bundle file changed"):
            self.manager["bundle"](root)

    def test_new_format_cannot_omit_guides_or_follow_a_document_directory_link(self):
        root = self.make_bundle(2)
        manifest_path = root / "bundle.json"
        original = manifest_path.read_text()
        manifest = json.loads(original)
        del manifest["files"]["docs/change-links.md"]
        manifest_path.write_text(json.dumps(manifest))
        with self.assertRaises(ValueError):
            self.manager["bundle"](root)
        manifest_path.write_text(original)
        (root / "docs").rename(self.root / "outside")
        (root / "docs").symlink_to(self.root / "outside", target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "regular directory"):
            self.manager["bundle"](root)


    def test_new_format_requires_portable_identity_helper_and_distinct_roles(self):
        root = self.make_bundle(3)
        path = root / "bundle.json"
        original = path.read_text()
        manifest = json.loads(original)
        del manifest["files"]["image_identity.py"]
        path.write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, "file set"):
            self.manager["bundle"](root)
        manifest = json.loads(original)
        manifest["edge_image"] = manifest["application_image"]
        path.write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, "distinct image roles"):
            self.manager["bundle"](root)
        manifest = json.loads(original)
        manifest["images"][0]["config_digest"] = "not-an-identity"
        path.write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, "invalid image digest"):
            self.manager["bundle"](root)


class DocumentationTests(unittest.TestCase):
    def test_native_and_server_guides_have_complete_offline_links(self):
        import posixpath
        import re
        from urllib.parse import unquote, urlsplit
        root = SOURCE.parent.parent
        native = runpy.run_path(str(root / "scripts/package-internal.py"))
        server = runpy.run_path(str(root / "scripts/package-server.py"))
        for payload in [native["PAYLOAD"], server["DOCUMENTS"]]:
            for destination, source in payload.items():
                if not destination.endswith(".md"):
                    continue
                text = native["document_bytes"](source, (root / source).read_bytes()).decode()
                for link in re.findall(r"\[[^\]]*\]\(([^)\s]+)\)", text):
                    url = urlsplit(link)
                    if url.scheme or url.netloc or not url.path:
                        continue
                    path = posixpath.normpath(posixpath.join(posixpath.dirname(destination), unquote(url.path)))
                    self.assertIn(path, payload, f"{destination}: offline link {link} has no packaged target")

    def test_operator_guide_is_part_of_the_build_identity(self):
        import shutil
        root = SOURCE.parent.parent
        package = runpy.run_path(str(root / "scripts/package-internal.py"))
        inventory = package["inputs"]()
        with tempfile.TemporaryDirectory(prefix="alt-package-input-test-") as directory:
            fixture = Path(directory)
            for row in inventory["entries"]:
                destination = fixture / row["path"]
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(root / row["path"], destination)
                destination.chmod(row["mode"])
            with patch.dict(package["inputs"].__globals__, {"ROOT": fixture}):
                before = package["inputs"]()
                guide = fixture / "crates/alt-server/README.md"
                guide.write_bytes(guide.read_bytes() + b"\nUpdated operator guidance.\n")
                after = package["inputs"]()
            self.assertNotEqual(before["sha256"], after["sha256"])
            changed = [a["path"] for a, b in zip(before["entries"], after["entries"]) if a != b]
            self.assertEqual(changed, ["crates/alt-server/README.md"])


IMAGEAPI = runpy.run_path(str(Path(__file__).resolve().parent.parent / 'self-hosted/image_identity.py'))


def encoded(value):
    return json.dumps(value, separators=(',', ':')).encode()


def digest(value):
    return 'sha256:' + hashlib.sha256(value).hexdigest()


def blob(value):
    return 'blobs/sha256/' + value[7:]


class ImageIdentityTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.path = Path(self.temp.name) / 'images.tar'
        self.layer = b'fixture-layer-bytes'
        self.layer_id = digest(self.layer)
        self.config = {'architecture': 'arm64', 'os': 'linux',
                       'config': {'User': '9000', 'Cmd': ['serve']},
                       'rootfs': {'type': 'layers', 'diff_ids': [self.layer_id]}}
        self.raw_config = encoded(self.config)
        self.config_id = digest(self.raw_config)
        self.manifest = {'schemaVersion': 2, 'mediaType': 'application/vnd.oci.image.manifest.v1+json',
                         'config': {'mediaType': 'application/vnd.oci.image.config.v1+json',
                                    'digest': self.config_id, 'size': len(self.raw_config)},
                         'layers': [{'mediaType': 'application/vnd.oci.image.layer.v1.tar',
                                     'digest': self.layer_id, 'size': len(self.layer)}]}
        self.raw_manifest = encoded(self.manifest)
        self.manifest_id = digest(self.raw_manifest)
        self.index = {'schemaVersion': 2, 'mediaType': 'application/vnd.oci.image.index.v1+json',
                      'manifests': [{'mediaType': self.manifest['mediaType'], 'digest': self.manifest_id,
                                     'size': len(self.raw_manifest), 'platform': {'os': 'linux', 'architecture': 'arm64'}}]}
        self.raw_index = encoded(self.index)
        self.index_id = digest(self.raw_index)
        self.rows = [{'Config': blob(self.config_id), 'RepoTags': ['alt:test'], 'Layers': [blob(self.layer_id)]}]
        self.files = {blob(self.config_id): self.raw_config, blob(self.layer_id): self.layer,
                      blob(self.manifest_id): self.raw_manifest, blob(self.index_id): self.raw_index}

    def tearDown(self):
        self.temp.cleanup()

    def write(self, extras=()):
        with tarfile.open(self.path, 'w') as tar:
            for name, raw in {**self.files, 'manifest.json': encoded(self.rows)}.items():
                entry = tarfile.TarInfo(name); entry.size = len(raw); tar.addfile(entry, io.BytesIO(raw))
            for entry, raw in extras:
                tar.addfile(entry, io.BytesIO(raw) if raw is not None else None)

    def test_config_manifest_index_domains_resolve_same_content(self):
        self.write()
        with IMAGEAPI['Archive'](self.path) as archive:
            identity = archive.for_reference('docker.io/library/alt:test', 'arm64')
            for value in [self.config_id, self.manifest_id, self.index_id]:
                self.assertEqual(archive.for_id(value, 'arm64'), identity)

    def test_origin_handle_cannot_be_replaced_by_different_configuration(self):
        self.config['config']['Cmd'] = ['different-command']
        self.files[blob(self.config_id)] = encoded(self.config)
        self.write()
        with IMAGEAPI['Archive'](self.path) as archive:
            for value in [self.config_id, self.manifest_id, self.index_id]:
                with self.assertRaises(ValueError): archive.for_id(value, 'arm64')

    def test_changed_manifest_with_old_name_is_rejected(self):
        self.manifest['config']['size'] += 1
        self.files[blob(self.manifest_id)] = encoded(self.manifest)
        self.write()
        with IMAGEAPI['Archive'](self.path) as archive:
            with self.assertRaisesRegex(ValueError, 'identity differs'):
                archive.for_id(self.manifest_id, 'arm64')

    def test_bad_config_extent_with_valid_new_manifest_digest_rejected(self):
        self.manifest['config']['size'] += 1
        value = encoded(self.manifest); bad_id = digest(value); self.files[blob(bad_id)] = value
        self.write()
        with IMAGEAPI['Archive'](self.path) as archive:
            with self.assertRaisesRegex(ValueError, 'descriptor differs'): archive.for_id(bad_id, 'arm64')

    def test_wrong_architecture_rejected(self):
        self.write()
        with IMAGEAPI['Archive'](self.path) as archive:
            with self.assertRaises(ValueError): archive.for_reference('alt:test', 'amd64')
            with self.assertRaises(ValueError): archive.for_id(self.index_id, 'amd64')

    def test_duplicate_tag_resolution_rejected(self):
        self.rows += copy.deepcopy(self.rows)
        self.write()
        with IMAGEAPI['Archive'](self.path) as archive:
            with self.assertRaises(ValueError): archive.for_reference('alt:test', 'arm64')

    def test_missing_layer_rejected(self):
        del self.files[blob(self.layer_id)];self.write()
        with self.assertRaisesRegex(ValueError, 'missing image archive layer'): IMAGEAPI['Archive'](self.path)

    def test_layer_count_mismatch_rejected(self):
        self.rows[0]['Layers'] = [];self.write()
        with self.assertRaisesRegex(ValueError, 'layer count'): IMAGEAPI['Archive'](self.path)

    def test_duplicate_archive_path_rejected(self):
        entry = tarfile.TarInfo('manifest.json');entry.size = 2
        self.write([(entry, b'[]')])
        with self.assertRaisesRegex(ValueError, 'duplicate'): IMAGEAPI['Archive'](self.path)

    def test_archive_link_rejected(self):
        entry = tarfile.TarInfo('elsewhere');entry.type = tarfile.SYMTYPE;entry.linkname = '/etc/passwd'
        self.write([(entry, None)])
        with self.assertRaises(ValueError): IMAGEAPI['Archive'](self.path)

    def test_archive_traversal_rejected(self):
        entry = tarfile.TarInfo('../elsewhere');entry.size = 0
        self.write([(entry, b'')])
        with self.assertRaises(ValueError): IMAGEAPI['Archive'](self.path)

    def test_oversized_config_rejected(self):
        self.files[blob(self.config_id)] = self.raw_config + b' ' * (2 * 1024 * 1024)
        self.write()
        with self.assertRaisesRegex(ValueError, 'oversized'): IMAGEAPI['Archive'](self.path)

    def test_duplicate_json_fields_rejected(self):
        self.files[blob(self.config_id)] = b'{"os":"linux","os":"other"}'
        self.write()
        with self.assertRaisesRegex(ValueError, 'duplicate image metadata'): IMAGEAPI['Archive'](self.path)

    def test_ambiguous_index_rejected(self):
        self.index['manifests'] *= 2
        raw = encoded(self.index); value = digest(raw);self.files[blob(value)] = raw
        self.write()
        with IMAGEAPI['Archive'](self.path) as archive:
            with self.assertRaises(ValueError): archive.for_id(value, 'arm64')

    def test_unknown_digest_rejected(self):
        self.write()
        with IMAGEAPI['Archive'](self.path) as archive:
            with self.assertRaises(ValueError): archive.for_id('sha256:' + 'f'*64, 'arm64')

    def test_format_three_declared_config_must_match(self):
        self.write()
        manifest = {'format': 3, 'architecture': 'arm64', 'images': [
            {'id': self.index_id, 'reference': 'alt:test', 'config_digest': self.config_id}]}
        self.assertEqual(IMAGEAPI['bundle_identities'](self.path, manifest)[self.index_id]['config_digest'], self.config_id)
        manifest['images'][0]['config_digest'] = 'sha256:' + '0'*64
        with self.assertRaises(ValueError): IMAGEAPI['bundle_identities'](self.path, manifest)



if __name__ == "__main__":
    unittest.main()
