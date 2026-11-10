#!/usr/bin/env python3
"""Manage a verified internal alt package in a dedicated, user-owned prefix."""

import argparse
import contextlib
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import stat
import sys
import tempfile


FORMAT = "alt-internal-package-v2"
OWNER = {"format": "alt-internal-install-v1"}
LEGACY_FILES = {
    "bin/alt", "bin/altd", "bin/altd-server", "install.py", "README.md",
    "LICENSE-MIT", "LICENSE-APACHE", "docs/internal-install.md",
    "docs/recovery.md", "docs/task-walkthrough.md",
    "examples/task-workflow.py",
}
FILES = LEGACY_FILES | {"docs/task-remote.md", "docs/change-links.md",
                        "server-operator.md", "example-config.json"}
PAYLOAD_FORMATS = {"alt-internal-package-v1": LEGACY_FILES, FORMAT: FILES}
TARGETS = {("Darwin", "arm64"): "aarch64-apple-darwin",
           ("Linux", "x86_64"): "x86_64-unknown-linux-gnu"}


def fail(message):
    raise ValueError(message)


def sha(path):
    h = hashlib.sha256()
    with path.open("rb") as src:
        for block in iter(lambda: src.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def regular(path):
    if not stat.S_ISREG(path.lstat().st_mode):
        fail(f"not a regular file: {path}")


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def read_json(path):
    regular(path)
    return json.loads(path.read_text())


def verify_package(root, check_platform=True):
    if root.is_symlink() or not root.is_dir():
        fail("package root must be a real directory")
    manifest = read_json(root / "manifest.json")
    files = PAYLOAD_FORMATS.get(manifest.get("format"))
    if files is None or set(manifest.get("files", {})) != files:
        fail("unsupported package format or payload inventory")
    if not re.fullmatch(r"0\.1\.0-internal\.[1-9][0-9]*", manifest.get("version", "")):
        fail("unsupported internal package version")
    target = TARGETS.get((platform.system(), platform.machine()))
    if check_platform and (target is None or manifest.get("target") != target):
        fail("package target does not match this operating system and architecture")
    expected = files | {"manifest.json"}
    directories = {"bin", "docs", "examples"}
    for path in root.rglob("*"):
        name = path.relative_to(root).as_posix()
        mode = path.lstat().st_mode
        if stat.S_ISDIR(mode) and name in directories:
            continue
        if name not in expected or not stat.S_ISREG(mode):
            fail(f"unexpected package entry: {name}")
    for name in sorted(files):
        path = root / name
        regular(path)
        item = manifest["files"][name]
        wanted_mode = 0o755 if name.startswith("bin/") else 0o644
        if (set(item) != {"sha256", "bytes", "mode"}
                or item["mode"] != wanted_mode
                or stat.S_IMODE(path.stat().st_mode) != wanted_mode
                or path.stat().st_size != item["bytes"] or sha(path) != item["sha256"]):
            fail(f"package file changed: {name}")
    return manifest, sha(root / "manifest.json")


def prefix_path(value):
    path = Path(value).expanduser().absolute()
    if path.is_symlink():
        fail("installation prefix must not be a symlink")
    # Resolve parent aliases before looking for repository ancestors or home.
    path = path.resolve()
    if path == path.parent or path == Path.home().resolve():
        fail("choose a dedicated real installation prefix, not a home or filesystem root")
    if not path.parent.is_dir():
        fail("installation prefix parent must already exist")
    for parent in [path, *path.parents]:
        if any((parent / marker).exists() or (parent / marker).is_symlink()
               for marker in [".git", ".alt"]):
            fail("installation prefix must be outside repositories")
    return path


@contextlib.contextmanager
def lock_prefix(prefix, create=False):
    if create and not prefix.exists():
        prefix.mkdir(mode=0o755)
        with (prefix / "owner.json").open("x") as dst:
            json.dump(OWNER, dst)
            dst.flush()
            os.fsync(dst.fileno())
        (prefix / "releases").mkdir()
        (prefix / "bin").symlink_to("current/bin", target_is_directory=True)
        sync_dir(prefix)
        sync_dir(prefix.parent)
    if prefix.is_symlink() or read_json(prefix / "owner.json") != OWNER:
        fail("prefix is not owned by this installer")
    flags = os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW
    fd = os.open(prefix / ".lock", flags, 0o600)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        here = (prefix / ".lock").stat()
        held = os.fstat(fd)
        if (here.st_dev, here.st_ino) != (held.st_dev, held.st_ino):
            fail("installation moved while waiting for its lock; inspect it again")
        yield
    finally:
        os.close(fd)


def installed(prefix):
    allowed = {"owner.json", ".lock", "bin", "current", "releases"}
    if {p.name for p in prefix.iterdir()} - allowed:
        fail("prefix contains unowned entries; preserving it unchanged")
    if not (prefix / "bin").is_symlink() or os.readlink(prefix / "bin") != "current/bin":
        fail("installation bin link changed")
    releases = prefix / "releases"
    if releases.is_symlink() or not releases.is_dir():
        fail("installation releases directory changed")
    versions = {}
    for path in releases.iterdir():
        if not re.fullmatch(r"[0-9a-f]{64}", path.name):
            fail("unowned release entry; preserving it unchanged")
        manifest, identity = verify_package(path)
        if path.name != identity:
            fail("installed release identity changed")
        versions[identity] = manifest["version"]
    current = prefix / "current"
    active = None
    if current.is_symlink():
        link = os.readlink(current)
        active = link.removeprefix("releases/")
        if link != "releases/" + active or active not in versions:
            fail("active release link changed or points to a missing release")
    elif current.exists():
        fail("current must be an installer-owned symlink")
    return {"active": active, "releases": versions}


def activate(prefix, identity):
    if not re.fullmatch(r"[0-9a-f]{64}", identity):
        fail("release identity must be 64 hexadecimal digits")
    state = installed(prefix)
    if identity not in state["releases"]:
        fail("release is not installed")
    # The temporary link stays outside the owned prefix. A killed switch leaves
    # either the old or the new current link, never a half-copied executable.
    with tempfile.TemporaryDirectory(prefix=".alt-activate-", dir=prefix.parent) as tmp:
        link = Path(tmp) / "current"
        link.symlink_to("releases/" + identity, target_is_directory=True)
        os.replace(link, prefix / "current")
        sync_dir(prefix)
    return installed(prefix)


def install(package, prefix):
    manifest, identity = verify_package(package)
    with tempfile.TemporaryDirectory(prefix=".alt-install-", dir=prefix.parent) as tmp:
        stage = Path(tmp) / "release"
        stage.mkdir()
        for name in sorted(set(manifest["files"]) | {"manifest.json"}):
            dst = stage / name
            dst.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(package / name, dst)
            dst.chmod(0o755 if name.startswith("bin/") else 0o644)
            with dst.open("rb") as src:
                os.fsync(src.fileno())
        if verify_package(stage)[1] != identity:
            fail("package changed while it was copied")
        for directory in [stage / "bin", stage / "docs", stage / "examples", stage]:
            sync_dir(directory)
        with lock_prefix(prefix, create=True):
            installed(prefix)
            destination = prefix / "releases" / identity
            if not destination.exists():
                os.rename(stage, destination)
                sync_dir(prefix / "releases")
            state = activate(prefix, identity)
    return {"prefix": str(prefix), "version": manifest["version"], **state}


def manage(prefix, action, identity=None):
    with lock_prefix(prefix):
        state = installed(prefix)
        if action == "status":
            return {"prefix": str(prefix), **state}
        if action == "activate":
            return {"prefix": str(prefix), **activate(prefix, identity)}
        # Validate every release and every owned path before moving the root.
        # Renaming separates this removal from a later fresh install at prefix.
        with tempfile.TemporaryDirectory(prefix=".alt-uninstall-", dir=prefix.parent) as tmp:
            quarantine = Path(tmp) / "installation"
            os.rename(prefix, quarantine)
            sync_dir(prefix.parent)
            shutil.rmtree(quarantine)
        return {"prefix": str(prefix), "removed": True, "releases": state["releases"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["verify", "install", "status", "activate", "uninstall"])
    parser.add_argument("--prefix", help="dedicated installation directory (never a repository)")
    parser.add_argument("--release", help="retained manifest identity for activate")
    args = parser.parse_args()
    if args.action == "activate" and not args.release:
        parser.error("activate requires --release")
    if args.action != "activate" and args.release:
        parser.error("--release is only valid for activate")
    try:
        if args.action == "verify":
            manifest, identity = verify_package(Path(__file__).absolute().parent)
            result = {"verified": True, "release": identity, "version": manifest["version"], "target": manifest["target"]}
        else:
            if not args.prefix:
                parser.error("this action requires --prefix")
            prefix = prefix_path(args.prefix)
            result = (install(Path(__file__).absolute().parent, prefix) if args.action == "install"
                      else manage(prefix, args.action, args.release))
        print(json.dumps(result, sort_keys=True))
    except (OSError, ValueError, KeyError, TypeError) as exc:
        print(json.dumps({"error": str(exc), "action": args.action}), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
