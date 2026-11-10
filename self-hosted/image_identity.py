"""Bind portable OCI configuration identity to a verified Docker-local handle.

Docker's classic store identifies images by config digest; containerd may use a
manifest/index digest instead. Never compare these different identity domains.
No archive is extracted, and no image is executed by this module.
"""
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import tarfile
import tempfile

DIGEST = re.compile(r'sha256:[a-f0-9]{64}\Z')
MANIFESTS = {'application/vnd.oci.image.manifest.v1+json',
             'application/vnd.docker.distribution.manifest.v2+json'}
INDEXES = {'application/vnd.oci.image.index.v1+json',
           'application/vnd.docker.distribution.manifest.list.v2+json'}


def digest(value):
    if not isinstance(value, str) or not DIGEST.fullmatch(value):
        raise ValueError('invalid image digest')
    return value


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('duplicate image metadata field')
        result[key] = value
    return result


def reference(value):
    if not isinstance(value, str) or not value:
        raise ValueError('invalid archive image reference')
    if value.startswith('docker.io/'):
        value = value[len('docker.io/'):]
    if value.startswith('library/'):
        value = value[len('library/'):]
    return value


class Archive:
    def __init__(self, path):
        self.tar = tarfile.open(path, 'r:*')
        self.members = {}
        self.configs = {}
        try:
            for member in self.tar:
                name = member.name.rstrip('/') if member.isdir() else member.name
                if (not name or PurePosixPath(name).is_absolute() or
                    any(part in ('', '.', '..') for part in name.split('/')) or
                    name in self.members or not (member.isfile() or member.isdir())):
                    raise ValueError('unsupported or duplicate image archive member')
                if len(self.members) >= 4096:
                    raise ValueError('too many image archive members')
                self.members[name] = member
            self.manifest, _ = self.read_json('manifest.json')
            if not isinstance(self.manifest, list) or not 1 <= len(self.manifest) <= 256:
                raise ValueError('invalid Docker archive manifest')
            for row in self.manifest:
                config, raw = self.read_json(row['Config'], 2 * 1024 * 1024)
                identity = self.configuration(config, raw)
                if not isinstance(row['Layers'], list) or len(row['Layers']) != len(identity['diff_ids']):
                    raise ValueError('archive layer count differs from configuration')
                for layer in row['Layers']:
                    if layer not in self.members or not self.members[layer].isfile():
                        raise ValueError('missing image archive layer')
                self.configs[identity['config_digest']] = identity
        except BaseException:
            self.tar.close()
            raise

    def close(self):
        self.tar.close()

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()

    def read_json(self, name, limit=8 * 1024 * 1024):
        member = self.members.get(name)
        if member is None or not member.isfile() or member.size > limit:
            raise ValueError('missing or oversized image metadata')
        stream = self.tar.extractfile(member)
        if stream is None:
            raise ValueError('missing image metadata content')
        with stream:
            raw = stream.read(limit + 1)
        if len(raw) != member.size or len(raw) > limit:
            raise ValueError('truncated or oversized image metadata')
        return json.loads(raw, object_pairs_hook=unique_object), raw

    @staticmethod
    def configuration(config, raw):
        root = config['rootfs']
        if root['type'] != 'layers' or not isinstance(root['diff_ids'], list) or len(root['diff_ids']) > 1024:
            raise ValueError('invalid image root filesystem identity')
        for value in root['diff_ids']:
            digest(value)
        if not isinstance(config['os'], str) or not isinstance(config['architecture'], str):
            raise ValueError('missing image platform')
        return {'config_digest': 'sha256:' + hashlib.sha256(raw).hexdigest(),
                'os': config['os'], 'architecture': config['architecture'],
                'diff_ids': root['diff_ids']}

    def for_reference(self, name, architecture):
        matches = []
        for row in self.manifest:
            if any(reference(tag) == reference(name) for tag in (row.get('RepoTags') or [])):
                config, raw = self.read_json(row['Config'], 2 * 1024 * 1024)
                matches.append(self.configuration(config, raw))
        if len(matches) != 1 or matches[0]['os'] != 'linux' or matches[0]['architecture'] != architecture:
            raise ValueError('archive reference is missing, ambiguous or the wrong platform')
        return matches[0]

    def for_id(self, value, architecture):
        """Resolve a config/manifest/index digest without trusting blob names."""
        digest(value)
        if value in self.configs:
            matches = [self.configs[value]]
        else:
            matches = self.walk(value, set(), 0)
        matches = [item for item in matches if item['os'] == 'linux' and item['architecture'] == architecture]
        if len(matches) != 1:
            raise ValueError('image handle has no unique requested platform')
        return matches[0]

    def walk(self, value, seen, depth):
        digest(value)
        if value in seen or depth > 8 or len(seen) > 64:
            raise ValueError('cyclic or oversized image descriptor graph')
        seen.add(value)
        document, raw = self.read_json('blobs/sha256/' + value[7:])
        if 'sha256:' + hashlib.sha256(raw).hexdigest() != value or document.get('schemaVersion') != 2:
            raise ValueError('image descriptor identity differs')
        media = document.get('mediaType')
        if media in MANIFESTS:
            child = document['config']
            config, encoded = self.read_json('blobs/sha256/' + digest(child['digest'])[7:], 2 * 1024 * 1024)
            identity = self.configuration(config, encoded)
            if identity['config_digest'] != child['digest'] or len(encoded) != child['size']:
                raise ValueError('image configuration descriptor differs')
            if identity['config_digest'] not in self.configs:
                raise ValueError('image descriptor is absent from Docker archive manifest')
            return [identity]
        if media in INDEXES:
            children = document['manifests']
            if not isinstance(children, list) or not 1 <= len(children) <= 64:
                raise ValueError('invalid image index')
            result = []
            for child in children:
                child_id = digest(child['digest'])
                _, encoded = self.read_json('blobs/sha256/' + child_id[7:])
                if len(encoded) != child['size']:
                    raise ValueError('image index child extent differs')
                result.extend(self.walk(child_id, seen, depth + 1))
            return result
        raise ValueError('unsupported image descriptor type')


def bundle_identities(path, manifest):
    expected = {}
    with Archive(path) as archive:
        for image in manifest['images']:
            identity = archive.for_reference(image['reference'], manifest['architecture'])
            if archive.for_id(image['id'], manifest['architecture']) != identity:
                raise ValueError('bundle origin handle differs from its archive reference')
            if manifest['format'] >= 3 and image.get('config_digest') != identity['config_digest']:
                raise ValueError('bundle portable image identity differs')
            if image['id'] in expected:
                raise ValueError('duplicate bundle image handle')
            expected[image['id']] = identity
    return expected


def resolve_loaded(directory, manifest):
    """Verify actual daemon images and return immutable local IDs per role.

The caller must first verify the bundle and complete images.tar checksum.
Docker's verified content store owns layer hash validation; the exact config
binds its ordered uncompressed layer digests and all execution parameters.
"""
    machine = subprocess.check_output(['docker', 'info', '--format', '{{.Architecture}}'], text=True).strip()
    architecture = {'x86_64': 'amd64', 'aarch64': 'arm64', 'amd64': 'amd64', 'arm64': 'arm64'}.get(machine)
    if architecture != manifest['architecture']:
        raise ValueError('bundle architecture does not match Docker host')
    expected = bundle_identities(Path(directory) / 'images.tar', manifest)
    observed = {}
    for image in manifest['images']:
        found = json.loads(subprocess.check_output(['docker', 'image', 'inspect', image['reference']], text=True))[0]
        digest(found['Id'])
        if found['Architecture'] != architecture or found['Os'] != 'linux':
            raise ValueError('loaded image platform differs from bundle')
        if found['RootFS']['Layers'] != expected[image['id']]['diff_ids']:
            raise ValueError('loaded image layer identity differs from bundle')
        observed[image['id']] = found['Id']
    # Read the daemon's exact serialized config, not a reconstructed inspect
    # JSON object that may omit fields. Use the immutable handles captured above
    # so a concurrent retag cannot redirect this verification or later execution.
    with tempfile.TemporaryDirectory(prefix='alt-image-identity-') as temporary:
        saved = Path(temporary) / 'loaded.tar'
        subprocess.run(['docker', 'image', 'save', '--output', str(saved), *sorted(set(observed.values()))], check=True)
        with Archive(saved) as archive:
            for origin, local in observed.items():
                if archive.for_id(local, architecture) != expected[origin]:
                    raise ValueError('loaded image configuration identity differs from bundle')
    fields = ['application_image', 'database_image', 'edge_image']
    if any(manifest[field] not in observed for field in fields) or len({manifest[field] for field in fields}) != 3:
        raise ValueError('bundle image roles differ')
    return {**{field: observed[manifest[field]] for field in fields},
            'images': [{'origin_id': origin, 'local_id': local, **expected[origin]} for origin, local in observed.items()]}
