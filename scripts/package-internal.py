#!/usr/bin/env python3
"""Build the native internal alt package; never package arbitrary cached binaries."""

import argparse
from datetime import datetime, timezone
import gzip
import hashlib
import json
import os
from pathlib import Path
import platform
import runpy
import shutil
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
import tomllib


ROOT = Path(__file__).resolve().parent.parent
MANAGER = runpy.run_path(str(ROOT / "scripts/install-internal.py"))
PAYLOAD = {
    "install.py": "scripts/install-internal.py",
    "README.md": "README.md",
    "LICENSE-MIT": "LICENSE-MIT",
    "LICENSE-APACHE": "LICENSE-APACHE",
    "docs/internal-install.md": "docs/internal-install.md",
    "docs/recovery.md": "docs/recovery.md",
    "docs/task-walkthrough.md": "docs/task-walkthrough.md",
    "docs/task-remote.md": "docs/task-remote.md",
    "docs/change-links.md": "docs/change-links.md",
    "server-operator.md": "crates/alt-server/README.md",
    "example-config.json": "crates/alt-server/example-config.json",
    "examples/task-workflow.py": "examples/task-workflow.py",
}


def document_bytes(source, raw):
    """Keep the same guide text, with local links for the offline layout."""
    text = raw.decode("utf-8")
    if source == "README.md":
        text = text.replace("(crates/alt-server/README.md", "(server-operator.md")
    elif source == "self-hosted/README.md":
        text = text.replace("(../crates/alt-server/README.md", "(server-operator.md")
    elif source == "crates/alt-server/README.md":
        text = text.replace("(../../docs/", "(docs/")
    elif source == "docs/task-remote.md":
        text = text.replace("(../crates/alt-server/README.md", "(../server-operator.md")
    return text.encode("utf-8")


def command(argv):
    result = subprocess.run(argv, cwd=ROOT, capture_output=True, check=True)
    return result.stdout.decode("utf-8").strip()


def inputs():
    # This identity covers the Rust workspace, embedded test fixtures, package
    # generator, and packaged documents. It is not a claim of a hermetic build.
    paths = {ROOT / name for name in PAYLOAD.values()}
    paths.update(ROOT / name for name in ["Cargo.toml", "Cargo.lock", "scripts/package-internal.py",
                                         "scripts/gate.sh", "scripts/test-gate.py", "scripts/test-package.py",
                                         "scripts/with-test-postgres.sh",
                                         "scripts/with-test-postgres-native.sh",
                                         "scripts/package-server.py",
                                         "crates/alt-server/example-config.json"])
    # The server's browser assets are executable product inputs, including the
    # independently versioned shared UI archive. Never fingerprint node_modules
    # or generated dist output as though it were the source of the build.
    for name in ["package.json", "bun.lock", "index.html", "tsconfig.json", "vite.config.ts"]:
        paths.add(ROOT / "workbench" / name)
    for area in ["src", "public", "scripts", "vendor"]:
        for path in (ROOT / "workbench" / area).rglob("*"):
            if path.is_symlink():
                raise ValueError(f"workbench source link is unsupported: {path}")
            if path.is_file():
                paths.add(path)
    for path in (ROOT / "vendor").rglob("*"):
        if path.is_symlink():
            raise ValueError(f"vendored source link is unsupported: {path}")
        if path.is_file():
            paths.add(path)
    for path in (ROOT / "self-hosted").rglob("*"):
        if path.is_symlink():
            raise ValueError(f"server distribution link is unsupported: {path}")
        if path.is_file() and '__pycache__' not in path.parts:
            paths.add(path)
    for name in [".cargo/config", ".cargo/config.toml", "rust-toolchain", "rust-toolchain.toml"]:
        if (ROOT / name).exists():
            paths.add(ROOT / name)
    for crate in (ROOT / "crates").iterdir():
        if not crate.is_dir() or crate.is_symlink():
            raise ValueError(f"unexpected crate directory: {crate.name}")
        paths.add(crate / "Cargo.toml")
        if (crate / "build.rs").exists():
            paths.add(crate / "build.rs")
        for area in ["src", "tests", "benches", "examples"]:
            tree = crate / area
            if tree.is_symlink():
                raise ValueError(f"source link is unsupported: {tree}")
            if tree.exists():
                for path in tree.rglob("*"):
                    if path.is_symlink():
                        raise ValueError(f"source link is unsupported: {path}")
                    if path.is_file():
                        paths.add(path)
    rows = []
    for path in sorted(paths):
        MANAGER["regular"](path)
        rows.append({"path": path.relative_to(ROOT).as_posix(), "bytes": path.stat().st_size,
                     "mode": stat.S_IMODE(path.stat().st_mode), "sha256": MANAGER["sha"](path)})
    digest = hashlib.sha256(json.dumps(rows, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    return {"sha256": digest, "entries": rows}


def check_binary(path, target):
    with path.open("rb") as binary:
        header = binary.read(20)
    if len(header) != 20:
        raise ValueError(f"truncated executable: {path.name}")
    if target == "x86_64-unknown-linux-gnu":
        valid = header[:6] == b"\x7fELF\x02\x01" and struct.unpack_from("<H", header, 18)[0] == 62
    else:
        valid = header[:4] == b"\xcf\xfa\xed\xfe" and struct.unpack_from("<I", header, 4)[0] == 0x100000C
    if not valid:
        raise ValueError(f"unexpected executable format for {target}: {path.name}")


def build(output):
    if output.exists() or output.is_symlink() or not output.parent.is_dir():
        raise ValueError("output must be a new archive in an existing directory")
    target = MANAGER["TARGETS"].get((platform.system(), platform.machine()))
    if not target:
        raise ValueError("internal packages support native macOS arm64 and Linux x86_64 only")
    rustc = command(["rustc", "-Vv"])
    if f"host: {target}" not in rustc or os.environ.get("CARGO_BUILD_TARGET", target) != target:
        raise ValueError("package requires the native Rust host target")
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    before = inputs()
    argv = ["cargo", "build", "--locked", "--release", "--package", "alt-cli", "--bins",
            "--message-format=json-render-diagnostics"]
    print("Building locked native alt executables…", file=sys.stderr, flush=True)
    result = subprocess.run(argv, cwd=ROOT, stdout=subprocess.PIPE, stderr=None, check=True)
    binaries = {}
    for line in result.stdout.splitlines():
        message = json.loads(line)
        if message.get("reason") == "compiler-artifact" and message.get("executable"):
            name = message["target"]["name"]
            if name in {"alt", "altd", "altd-server"}:
                binaries[name] = Path(message["executable"])
    if set(binaries) != {"alt", "altd", "altd-server"}:
        raise ValueError("Cargo did not identify all three executables")
    for path in binaries.values():
        check_binary(path, target)
    if command([str(binaries["alt"]), "--version"]) != f"alt {version}":
        raise ValueError("CLI version does not match the source package version")
    if inputs() != before:
        raise ValueError("source or package inputs changed during the build")
    build_vars = {k: v for k, v in os.environ.items()
                  if k in {"RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CC", "CFLAGS", "AR", "MACOSX_DEPLOYMENT_TARGET"}
                  or k.startswith("CARGO_PROFILE_RELEASE_")}
    with tempfile.TemporaryDirectory(prefix=".alt-package-", dir=output.parent) as tmp:
        temporary = Path(tmp)
        name = f"alt-{version}-{target}"
        package = temporary / name
        package.mkdir()
        sources = {**PAYLOAD, **{f"bin/{key}": str(path) for key, path in binaries.items()}}
        inventory = {}
        for relative, source in sorted(sources.items()):
            destination = package / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            if relative.endswith(".md"):
                destination.write_bytes(document_bytes(source, (ROOT / source).read_bytes()))
            else:
                shutil.copyfile(ROOT / source, destination)
            mode = 0o755 if relative.startswith("bin/") else 0o644
            destination.chmod(mode)
            inventory[relative] = {"sha256": MANAGER["sha"](destination), "bytes": destination.stat().st_size, "mode": mode}
        # Recheck both source and executable identities after copying, too.
        if inputs() != before or any(MANAGER["sha"](path) != inventory[f"bin/{key}"]["sha256"] for key, path in binaries.items()):
            raise ValueError("build inputs or executables changed during packaging")
        manifest = {"format": MANAGER["FORMAT"], "version": version, "target": target,
                    "files": inventory, "build": {"source": before, "command": argv,
                    "rustc": rustc, "cargo": command(["cargo", "--version"]),
                    "system": platform.platform(), "libc": list(platform.libc_ver()),
                    "environment": build_vars, "finished_at": datetime.now(timezone.utc).isoformat()}}
        (package / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
        (package / "manifest.json").chmod(0o644)
        _, identity = MANAGER["verify_package"](package)
        archive = temporary / "package.tar.gz"
        with archive.open("wb") as dst:
            with gzip.GzipFile(fileobj=dst, mode="wb", filename="", mtime=0) as compressed:
                with tarfile.open(fileobj=compressed, mode="w") as tar:
                    for path in sorted(package.rglob("*")):
                        if path.is_dir():
                            continue
                        info = tar.gettarinfo(str(path), arcname=f"{name}/{path.relative_to(package).as_posix()}")
                        info.uid = info.gid = info.mtime = 0
                        info.uname = info.gname = ""
                        with path.open("rb") as src:
                            tar.addfile(info, src)
            dst.flush()
            os.fsync(dst.fileno())
        # Hard-link publication is atomic and refuses an existing file or link.
        os.link(archive, output)
        MANAGER["sync_dir"](output.parent)
        return {"archive": str(output), "sha256": MANAGER["sha"](output), "bytes": output.stat().st_size,
                "release": identity, "version": version, "target": target, "source_sha256": before["sha256"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True, help="new .tar.gz archive path")
    args = parser.parse_args()
    try:
        print(json.dumps(build(args.output.expanduser().absolute()), sort_keys=True))
        return 0
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as exc:
        if isinstance(exc, subprocess.CalledProcessError) and exc.stdout:
            sys.stderr.buffer.write(exc.stdout)
        if isinstance(exc, subprocess.CalledProcessError) and exc.stderr:
            sys.stderr.buffer.write(exc.stderr)
        print(f"package-internal: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
