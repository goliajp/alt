#!/usr/bin/env python3
"""Install acceptance for one offline bundle: load, prepare, initialize, then push and
clone with a stock git client. Leaves nothing behind on success or failure."""
import argparse
import base64
import json
import os
from pathlib import Path
import secrets
import shutil
import subprocess
import tempfile
import time


def run(command, cwd=None, env=None, capture=True):
    result = subprocess.run([str(part) for part in command], cwd=cwd, env=env, capture_output=capture, text=True)
    if result.returncode:
        raise RuntimeError(f'{command[:5]} failed: {(result.stderr or "")[-2000:]}')
    return result.stdout if capture else None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bundle', type=Path, required=True)
    parser.add_argument('--expected-version', required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--port', type=int, default=18443)
    args = parser.parse_args()
    bundle = args.bundle.resolve()
    manage = bundle / 'manage.py'
    manifest = json.loads((bundle / 'bundle.json').read_text())
    work = Path(tempfile.mkdtemp(prefix='alt-install-smoke-'))
    directory = work / 'instance'
    project = 'alt-smoke-' + secrets.token_hex(4)
    report = {'bundle_manifest': manifest['source_digest'], 'architecture': manifest['architecture'], 'checks': []}
    prepared = False
    try:
        run(['openssl', 'req', '-x509', '-newkey', 'ec', '-pkeyopt', 'ec_paramgen_curve:prime256v1', '-nodes',
             '-keyout', work / 'key.pem', '-out', work / 'cert.pem', '-days', '2', '-subj', '/CN=alt.smoke.test',
             '-addext', 'subjectAltName=DNS:alt.smoke.test,IP:127.0.0.1'])
        run(['python3', manage, 'load', '--bundle', bundle])
        run(['python3', manage, 'prepare', '--bundle', bundle, '--directory', directory, '--project', project,
             '--origin', 'https://alt.smoke.test', '--https-port', args.port,
             '--certificate', work / 'cert.pem', '--private-key', work / 'key.pem'])
        prepared = True
        run(['python3', manage, 'initialize', '--directory', directory])
        report['checks'].append('load, prepare and initialize')

        def operator(*arguments):
            out = run(['python3', manage, 'operator', '--directory', directory, '--', *arguments])
            return json.loads(out.strip().splitlines()[-1])

        health = operator('health')
        assert health['ready'] and health['build']['version'] == args.expected_version, health
        assert health['build']['build_id'] == manifest['build_id'], health
        report['build'] = health['build']
        report['checks'].append('operator health reports the expected version and build')

        owner = (directory / 'operator').stat()
        def private(name, text):
            path = directory / 'operator' / name
            path.write_text(text)
            path.chmod(0o600)
            if os.geteuid() == 0:
                os.chown(path, owner.st_uid, owner.st_gid)
        private('grants.json', json.dumps({'read': True, 'publish_records': True, 'grants': [
            {'refs': {'Namespace': 'refs/'}, 'create': True, 'update': True, 'delete': True, 'force': True, 'paths': None}]}))
        token = 'alt1_' + secrets.token_hex(32)
        private('smoke.token', token + '\n')
        operator('repository-create', 'smoke', '--quota-bytes', str(1 << 30))
        operator('grants', 'smoke', 'owner', '--file', '/operator/grants.json')
        operator('token-issue', 'owner', '--secret', '/operator/smoke.token', '--expires-unix-ms', str(int(time.time() * 1000) + 3_600_000))

        url = f'https://127.0.0.1:{args.port}/git/smoke'
        auth = base64.b64encode(('alt:' + token).encode()).decode()
        env = {**os.environ, 'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_TERMINAL_PROMPT': '0',
               'GIT_CONFIG_COUNT': '2', 'GIT_CONFIG_KEY_0': 'http.extraHeader', 'GIT_CONFIG_VALUE_0': 'Authorization: Basic ' + auth,
               'GIT_CONFIG_KEY_1': 'http.sslVerify', 'GIT_CONFIG_VALUE_1': 'false',
               'GIT_AUTHOR_NAME': 'smoke', 'GIT_AUTHOR_EMAIL': 'smoke@example.test',
               'GIT_COMMITTER_NAME': 'smoke', 'GIT_COMMITTER_EMAIL': 'smoke@example.test'}
        first = work / 'first'
        first.mkdir()
        run(['git', 'init', '-q', '--initial-branch=main'], first, env)
        # larger than git's default post buffer, so the client probes and then sends a chunked request
        asset = secrets.token_bytes(3 * 1024 * 1024)
        (first / 'asset.bin').write_bytes(asset)
        (first / 'note.txt').write_text('one\n')
        run(['git', 'add', '.'], first, env)
        run(['git', 'commit', '-qm', 'first'], first, env)
        run(['git', 'push', '-q', url, 'main'], first, env)
        run(['git', 'clone', '-q', url, work / 'second'], work, env)
        assert (work / 'second/asset.bin').read_bytes() == asset
        run(['git', 'fsck', '--strict', '--full'], work / 'second', env)
        (first / 'note.txt').write_text('two\n')
        run(['git', 'commit', '-qam', 'second'], first, env)
        run(['git', 'push', '-q', url, 'main'], first, env)
        run(['git', 'pull', '-q', '--ff-only', 'origin', 'main'], work / 'second', env)
        head = run(['git', 'rev-parse', 'HEAD'], first, env).strip()
        assert run(['git', 'rev-parse', 'HEAD'], work / 'second', env).strip() == head
        report['checks'].append('stock git: large push, clone, fsck, incremental push and pull')
        report['passed'] = True
    finally:
        if prepared:
            subprocess.run(['docker', 'compose', '--env-file', str(directory / '.env'), '-f', str(directory / 'compose.yaml'),
                            'down', '--volumes', '--remove-orphans'], capture_output=True)
            # files created by the application user inside the instance
            subprocess.run(['docker', 'run', '--rm', '--user', '0', '--entrypoint', '/bin/rm', '-v', f'{directory}:/target',
                            manifest['application_image'], '-rf', '/target/data'], capture_output=True)
        shutil.rmtree(work, ignore_errors=True)
        args.output.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report))


if __name__ == '__main__':
    main()
