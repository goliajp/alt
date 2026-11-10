#!/usr/bin/env python3
"""Prepare and operate an explicit local alt installation. Never auto-migrate."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import runpy
import secrets
import ssl
import subprocess
import sys
import time
from urllib.parse import urlsplit

HERE = Path(__file__).resolve().parent
IMAGES = runpy.run_path(str(HERE / 'image_identity.py'))


def regular(path):
    if path.is_symlink() or not path.is_file():
        raise ValueError(f"expected regular file: {path}")
    return path


def sha(path):
    with regular(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def bundle(directory):
    manifest = json.loads(regular(directory / 'bundle.json').read_text())
    if manifest['format'] not in (1, 2, 3) or manifest['product'] != 'alt-server':
        raise ValueError('unsupported server bundle')
    required = {'images.tar', 'compose.yaml', 'Caddyfile', 'manage.py', 'example-config.json', 'README.md', 'dependencies.json', 'LICENSE-MIT', 'LICENSE-APACHE', 'server-operator.md'}
    if manifest['format'] >= 2:
        required |= {'docs/task-remote.md', 'docs/change-links.md'}
    if manifest['format'] >= 3:
        required.add('image_identity.py')
    if set(manifest['files']) != required:
        raise ValueError('bundle file set is incomplete')
    for name in required:
        for parent in (directory / name).parents:
            if parent == directory:
                break
            if parent.is_symlink() or not parent.is_dir():
                raise ValueError('bundle document directory is not a regular directory')
    for name, digest in manifest['files'].items():
        if name not in required:
            raise ValueError('unexpected bundle path')
        if sha(directory / name) != digest:
            raise ValueError(f'bundle file changed: {name}')
    if manifest['architecture'] not in ('amd64', 'arm64') or len(manifest['images']) != 3:
        raise ValueError('unsupported bundle platform or image count')
    roles = [manifest[field] for field in ['application_image', 'database_image', 'edge_image']]
    if len(set(roles)) != 3:
        raise ValueError('bundle requires three distinct image roles')
    for value in roles:
        IMAGES['digest'](value)
    if {image['id'] for image in manifest['images']} != set(roles):
        raise ValueError('bundle images do not match their roles')
    for image in manifest['images']:
        if not isinstance(image['reference'], str) or not re.fullmatch(r'[a-z0-9][A-Za-z0-9._:/@-]{0,254}', image['reference']):
            raise ValueError('invalid bundle image reference')
        if manifest['format'] >= 3:
            IMAGES['digest'](image['config_digest'])
    return manifest


def write(path, value, uid, gid):
    raw = value.encode() if isinstance(value, str) else value
    with path.open('xb') as out:
        os.chmod(path, 0o600)
        out.write(raw)
        out.flush()
        os.fsync(out.fileno())
    if os.geteuid() == 0:
        os.chown(path, uid, gid)


def prepare(args):
    source = args.bundle.resolve()
    manifest = bundle(source)
    target = args.directory.absolute()
    if target.exists() or target.is_symlink():
        raise ValueError('installation destination must not exist')
    if not re.fullmatch(r'[a-z][a-z0-9_-]{0,48}', args.project):
        raise ValueError('project must be a lowercase Compose project identity')
    origin = urlsplit(args.origin)
    if origin.scheme != 'https' or not origin.hostname or origin.username or origin.password or origin.path not in ['', '/'] or origin.query or origin.fragment:
        raise ValueError('public origin must be an explicit HTTPS origin')
    # Let the same server validator check canonical spelling at initialization.
    public_origin = args.origin.rstrip('/')
    uid = args.uid if args.uid is not None else (os.geteuid() or 9000)
    gid = args.gid if args.gid is not None else (os.getegid() or 9000)
    if uid <= 0 or gid <= 0:
        raise ValueError('application and TLS edge require non-root uid/gid')
    if os.geteuid() != 0 and (uid != os.geteuid() or gid != os.getegid()):
        raise ValueError('preparing another uid/gid requires root')
    certificate = regular(args.certificate).read_bytes()
    private_key = regular(args.private_key).read_bytes()
    # Verify that the supplied certificate/key pair can actually serve TLS.
    ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER).load_cert_chain(args.certificate, args.private_key)
    local_images = IMAGES['resolve_loaded'](source, manifest)
    target.mkdir(mode=0o700)
    for name in ['config', 'secrets', 'operator', 'tls', 'data']:
        path = target / name
        path.mkdir(mode=0o700)
        if os.geteuid() == 0:
            os.chown(path, uid, gid)
    for name in ['compose.yaml', 'Caddyfile']:
        write(target / name, (source / name).read_bytes(), uid, gid)
    write(target / 'tls/certificate.pem', certificate, uid, gid)
    write(target / 'tls/private-key.pem', private_key, uid, gid)
    password = secrets.token_hex(32)
    write(target / 'secrets/database-password', password + '\n', uid, gid)
    write(target / 'secrets/database', f'host=/var/run/postgresql user=alt dbname=alt password={password}\n', uid, gid)
    write(target / 'secrets/receipt.key', secrets.token_bytes(32), uid, gid)
    write(target / 'operator/admin.token', 'alt1_' + secrets.token_hex(32) + '\n', uid, gid)
    write(target / 'operator/initial-password', secrets.token_urlsafe(32) + '\n', uid, gid)
    config = json.loads((source / 'example-config.json').read_text())
    config['instance']['installation_id'] = 'alt-' + secrets.token_hex(16)
    config['instance']['workspace_id'] = 'workspace-' + secrets.token_hex(16)
    config['bytes_directory'] = '/var/lib/alt'
    config['receipt_key_file'] = '/run/alt/receipt.key'
    config['browser'] = {'public_origin': public_origin, 'assets_directory': '/opt/alt/workbench', 'session_ttl_ms': 3_600_000}
    write(target / 'config/config.json', json.dumps(config, indent=2) + '\n', uid, gid)
    write(target / '.env', f'COMPOSE_PROJECT_NAME={args.project}\nALT_IMAGE={local_images["application_image"]}\nALT_DATABASE_IMAGE={local_images["database_image"]}\nALT_EDGE_IMAGE={local_images["edge_image"]}\nALT_UID={uid}\nALT_GID={gid}\nALT_BIND_ADDRESS={args.bind}\nALT_HTTPS_PORT={args.https_port or origin.port or 443}\n', uid, gid)
    write(target / 'bundle-identity.json', json.dumps({'source_digest': manifest['source_digest'], 'architecture': manifest['architecture'], 'bundle_manifest_sha256': sha(source / 'bundle.json'), **local_images}, indent=2) + '\n', uid, gid)
    if os.geteuid() == 0:
        os.chown(target, uid, gid)
    print(json.dumps({'prepared': str(target), 'initialized': False, 'installation_id': config['instance']['installation_id'], 'login': 'owner', 'password_file': str(target / 'operator/initial-password')}))


def compose(directory, *args):
    directory = directory.resolve(strict=True)
    regular(directory / '.env')
    regular(directory / 'compose.yaml')
    subprocess.run(['docker', 'compose', '--project-directory', str(directory), '--env-file', str(directory / '.env'), '-f', str(directory / 'compose.yaml'), *args], check=True)


def operator(directory, arguments, mounts=()):
    # This directory is mounted only for explicit operator jobs, never serve.
    options = ['run', '--rm', '--no-deps', '-v', f'{directory.resolve() / "operator"}:/operator:ro']
    for mount in mounts:
        options += ['-v', mount]
    compose(directory, *options, 'app', '--credential', '/operator/admin.token', *arguments)


def initialize(args):
    directory = args.directory.resolve(strict=True)
    compose(directory, 'up', '-d', '--wait', 'database', 'edge')
    # Each operation uses the same server implementation as a standalone
    # operator. Failure preserves state for inspection; no destructive cleanup
    # or implicit retry is attempted.
    operator(directory, ['initialize'])
    operator(directory, ['bootstrap', 'owner', '--secret', '/operator/admin.token', '--expires-unix-ms', str(int(time.time()*1000) + 3_600_000)])
    operator(directory, ['browser-enable'])
    operator(directory, ['user-create', 'owner', '--login', 'owner', '--display-name', 'Owner', '--password-file', '/operator/initial-password', '--administrator'])
    compose(directory, 'up', '-d', '--wait', 'app')
    print(json.dumps({'initialized': True, 'password_file': str(directory / 'operator/initial-password'), 'bootstrap_token_lifetime_seconds': 3600}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    load = sub.add_parser('load', help='verify a trusted offline bundle and load its images')
    load.add_argument('--bundle', type=Path, required=True)
    prep = sub.add_parser('prepare', help='prepare a new empty installation directory')
    prep.add_argument('--bundle', type=Path, required=True)
    prep.add_argument('--directory', type=Path, required=True)
    prep.add_argument('--project', required=True)
    prep.add_argument('--origin', required=True)
    prep.add_argument('--certificate', type=Path, required=True)
    prep.add_argument('--private-key', type=Path, required=True)
    prep.add_argument('--bind', default='127.0.0.1', choices=['127.0.0.1', '0.0.0.0'])
    prep.add_argument('--https-port', type=int, help='host port of the HTTPS edge when a front proxy owns the public port (default: the port of --origin)')
    prep.add_argument('--uid', type=int)
    prep.add_argument('--gid', type=int)
    init = sub.add_parser('initialize', help='initialize explicitly; never resume or migrate on restart')
    init.add_argument('--directory', type=Path, required=True)
    op = sub.add_parser('operator', help='run explicit server operator commands')
    op.add_argument('--directory', type=Path, required=True)
    op.add_argument('--mount', action='append', default=[], help='explicit Docker bind mount for input/output artifacts')
    op.add_argument('arguments', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if args.command == 'load':
        manifest = bundle(args.bundle)
        machine = subprocess.check_output(['docker', 'info', '--format', '{{.Architecture}}'], text=True).strip()
        architecture = {'x86_64': 'amd64', 'aarch64': 'arm64', 'amd64': 'amd64', 'arm64': 'arm64'}.get(machine)
        if architecture != manifest['architecture']:
            raise ValueError('bundle architecture does not match Docker host')
        IMAGES['bundle_identities'](args.bundle / 'images.tar', manifest)
        subprocess.run(['docker', 'load', '--input', str(args.bundle / 'images.tar')], check=True)
        local_images = IMAGES['resolve_loaded'](args.bundle, manifest)
        print(json.dumps({'loaded': True, **local_images}))
    elif args.command == 'prepare':
        prepare(args)
    elif args.command == 'initialize':
        initialize(args)
    else:
        arguments = args.arguments[1:] if args.arguments[:1] == ['--'] else args.arguments
        if not arguments:
            raise ValueError('operator command is required')
        operator(args.directory, arguments, args.mount)


if __name__ == '__main__':
    try:
        main()
    except (ValueError, OSError, KeyError, subprocess.CalledProcessError) as error:
        print(f'Operation stopped: {error}. Existing installation data was preserved; inspect the original operation before retrying.', file=sys.stderr)
        sys.exit(1)
