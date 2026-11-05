#!/usr/bin/env python3
"""Build a frozen server image and an offline, architecture-specific Compose bundle."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import runpy
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parent.parent
SOURCE = runpy.run_path(str(ROOT / 'scripts/package-internal.py'))


def run(*args, capture=False):
    result = subprocess.run(list(args), cwd=ROOT, check=True, text=True, stdout=subprocess.PIPE if capture else None)
    return result.stdout.strip() if capture else None


def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def build(args):
    output = args.output.absolute()
    if output.exists() or output.is_symlink() or not output.parent.is_dir():
        raise ValueError('output must be a new directory inside an existing parent')
    before = SOURCE['inputs']()
    digest = before['sha256']
    platform = 'linux/' + args.architecture
    tag = f'alt-server:source-{digest[:24]}-{args.architecture}'
    build_id = 'sha256-' + digest
    run('docker', 'buildx', 'build', '--load', '--platform', platform,
        '--build-arg', 'ALT_BUILD_ID=' + build_id,
        '--build-arg', 'CARGO_BUILD_JOBS=' + str(args.jobs),
        '--file', 'self-hosted/Dockerfile', '--tag', tag, '.')
    info = json.loads(run('docker', 'image', 'inspect', tag, capture=True))[0]
    if info['Architecture'] != args.architecture or info['Config']['Labels']['org.opencontainers.image.revision'] != build_id:
        raise ValueError('built image identity mismatch')
    # Only a native runtime check is evidence of execution. Cross-built images
    # retain an explicit untested marker until acceptance on their target host.
    native = {'x86_64': 'amd64', 'aarch64': 'arm64', 'amd64': 'amd64', 'arm64': 'arm64'}.get(run('docker', 'info', '--format', '{{.Architecture}}', capture=True))
    runtime_checked = native == args.architecture
    if runtime_checked:
        actual = json.loads(run('docker', 'run', '--rm', '--network', 'none', '--read-only', '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges:true', info['Id'], 'build-info', capture=True))
        if actual['build_id'] != build_id or actual['product'] != 'alt-server':
            raise ValueError('runtime binary identity differs from source')
    images = [{'reference': tag, 'id': info['Id'], 'source': 'local-checkout'}]
    named = {'application_image': info['Id']}
    dependencies = json.loads((ROOT / 'self-hosted/dependencies.json').read_text())
    for name, field in [('postgres', 'database_image'), ('caddy', 'edge_image')]:
        pin = next(item for item in dependencies['images'] if item['repository'] == 'library/' + name)
        reference = f'{name}:{pin["tag"]}@{pin["manifests"][args.architecture]}'
        run('docker', 'pull', '--platform', platform, reference)
        metadata = json.loads(run('docker', 'image', 'inspect', reference, capture=True))[0]
        if metadata['Architecture'] != args.architecture:
            raise ValueError('dependency architecture differs from candidate')
        local = f'alt-bundle/{name}:{args.architecture}-{pin["digest"][7:23]}'
        run('docker', 'tag', metadata['Id'], local)
        images.append({'reference': local, 'id': metadata['Id'], 'source': reference})
        named[field] = metadata['Id']
    if SOURCE['inputs']() != before:
        raise ValueError('source changed during image build; no bundle published')
    output.mkdir(mode=0o700)
    run('docker', 'image', 'save', '--output', str(output / 'images.tar'), *[image['reference'] for image in images])
    files = {'images.tar': sha(output / 'images.tar')}
    for name in ['compose.yaml', 'Caddyfile', 'manage.py', 'README.md', 'dependencies.json']:
        if name == 'README.md':
            (output / name).write_text((ROOT / 'self-hosted' / name).read_text().replace('../crates/alt-server/README.md', 'server-operator.md'))
        else:
            shutil.copyfile(ROOT / 'self-hosted' / name, output / name)
        files[name] = sha(output / name)
    shutil.copyfile(ROOT / 'crates/alt-server/README.md', output / 'server-operator.md')
    files['server-operator.md'] = sha(output / 'server-operator.md')
    for name in ['LICENSE-MIT', 'LICENSE-APACHE']:
        shutil.copyfile(ROOT / name, output / name)
        files[name] = sha(output / name)
    shutil.copyfile(ROOT / 'crates/alt-server/example-config.json', output / 'example-config.json')
    files['example-config.json'] = sha(output / 'example-config.json')
    if SOURCE['inputs']() != before:
        raise ValueError('source changed during bundle creation; no completion manifest published')
    manifest = {'format': 1, 'product': 'alt-server', 'architecture': args.architecture,
                'source_digest': digest, 'build_id': build_id, 'native_build_info_checked': runtime_checked,
                **named, 'images': images, 'files': files}
    # The completion manifest is last; a half-written directory is not a bundle.
    with (output / 'bundle.json').open('x') as stream:
        json.dump(manifest, stream, indent=2)
        stream.write('\n')
        stream.flush()
        os.fsync(stream.fileno())
    print(json.dumps({'bundle': str(output), 'source_digest': digest, 'manifest_sha256': sha(output / 'bundle.json'), 'architecture': args.architecture, 'native_build_info_checked': runtime_checked}))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--architecture', choices=['amd64', 'arm64'], required=True)
    parser.add_argument('--jobs', type=int, default=2)
    args = parser.parse_args()
    if not 1 <= args.jobs <= 8:
        parser.error('--jobs must be between 1 and 8')
    try:
        build(args)
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f'package-server: {error}', file=sys.stderr)
        sys.exit(1)
