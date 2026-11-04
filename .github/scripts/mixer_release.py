"""Publish once and reuse a versioned mixer payload independently of desktop versions."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import re
import subprocess
import tarfile
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[2]
OUTPUT = ROOT / 'target/release-input'
SCRIPTS = ('rc2fx.sh', 'rc2fx-shim.sh', 'rc2fx-gui-shim.sh', 'install.sh')
RUNTIME = ('librc2fx_native.so', 'rc2fxd', 'rc2fx-firmware', 'firmware-profiles.json',
           'librc2fx_gui.so', 'librc2fx_jackerr.so', 'rc2fx_jackerr_snapshot',
           'mixer-release.json', 'SHA256SUMS')


def digest(data):
    return hashlib.sha256(data).hexdigest()


def identity(root=ROOT):
    value = json.loads((root / 'config/mixer-release.json').read_text())
    if not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', value['version']):
        raise ValueError('Mixer version must be stable major.minor.patch')
    return value


def normalized(path, data):
    # Desktop release preparation bumps local crate metadata, not mixer identity.
    if path.name in ('Cargo.toml', 'Cargo.lock'):
        value = tomllib.loads(data.decode())
        if path.name == 'Cargo.toml':
            value.get('package', {}).pop('version', None)
            value.get('workspace', {}).get('package', {}).pop('version', None)
        else:
            for package in value.get('package', []):
                if 'source' not in package:
                    package.pop('version', None)
        return json.dumps(value, sort_keys=True).encode()
    data = data.replace(b'\r\n', b'\n')
    if path.name == 'rc2fx.sh':
        data = data.replace(b'\nrc2fx_build=release\n', b'\nrc2fx_build=development\n')
    return data


def runtime_lock(data, roots):
    """Keep only the dependency closure shipped by this mixer workspace."""
    packages = tomllib.loads(data.decode()).get('package', [])
    selected = {}
    pending = list(roots)
    while pending:
        dependency = pending.pop()
        parts = dependency.split(' ')
        matches = [p for p in packages if p['name'] == parts[0]
                   and (len(parts) < 2 or p['version'] == parts[1])]
        if not matches:
            raise ValueError(f'Missing locked mixer dependency {dependency}')
        for package in matches:
            key = (package['name'], package['version'], package.get('source', ''))
            if key in selected:
                continue
            selected[key] = dict(package)
            pending.extend(package.get('dependencies', []))
    for package in selected.values():
        if 'source' not in package:
            package.pop('version', None)
    return json.dumps(sorted(selected.values(), key=lambda p: (p['name'], p.get('version', ''), p.get('source', ''))), sort_keys=True).encode()


def source_digest(root=ROOT):
    # Conservative: shared crates/build configuration are mixer inputs too.
    roots = ['Cargo.toml', 'Cargo.lock', '.cargo', 'rust-toolchain.toml',
             'config', 'about.toml', 'LICENSE-MIT', 'licenses',
             'THIRD_PARTY_NOTICES.md', 'xtask/mixer', 'xtask/src/setup.rs', 'xtask/src/elf.rs']
    roots += [str(p.relative_to(root)) for p in (root / 'crates').iterdir()
              if p.is_dir() and p.name not in ('rc2fx-app', 'rc2fx-installer', 'rc2fx-transport', 'rc2fx-bench')]
    tracked = subprocess.check_output(['git', 'ls-files', '-z', '--', *roots], cwd=root).decode().split('\0')
    entries = []
    for name in sorted(filter(None, tracked)):
        path = root / name
        data = path.read_bytes()
        if name == 'Cargo.lock':
            data = runtime_lock(data, ['rc2fx-device', 'rc2fx-firmware'])
        elif name == 'crates/rc2fx-native/Cargo.lock':
            data = runtime_lock(data, ['rc2fx-native'])
        else:
            data = normalized(path, data)
        entries.append(f'{digest(data)}  {name}\n')
    return digest(''.join(entries).encode())


def names(version):
    return f'rc2fx-mixer-{version}.tar.gz', f'rc2fx-mixer-{version}.json'


def release_info(tag):
    repo = os.environ['GH_REPO']
    result = subprocess.run(['gh', 'api', f'repos/{repo}/releases/tags/{tag}'], capture_output=True, text=True)
    if result.returncode:
        if '(HTTP 404)' in result.stderr:
            return None
        raise RuntimeError(result.stderr)
    return json.loads(result.stdout)


def validate_archive(archive_path, metadata, root=ROOT):
    if digest(archive_path.read_bytes()) != metadata['sha256']:
        raise ValueError('Mixer archive checksum mismatch')
    if metadata['identity'] != identity(root):
        raise ValueError('Mixer release identity differs from checkout')
    if metadata['sourceSha256'] != source_digest(root):
        raise ValueError('Mixer sources changed: bump config/mixer-release.json version before releasing')
    with tarfile.open(archive_path, 'r:gz') as archive:
        members = archive.getmembers()
        names_seen = set()
        for member in members:
            name = member.name
            allowed = (name in [f'runtime/{n}' for n in RUNTIME]
                       or name in [f'xtask/mixer/{s}' for s in SCRIPTS]
                       or name in ('config/mixer-release.json', 'config/firmware-profiles.json', 'LICENSES.html'))
            if (not allowed or not member.isfile() or '..' in Path(name).parts
                    or name in names_seen or member.size > 256 * 1024 * 1024):
                raise ValueError(f'Invalid mixer archive member: {name}')
            names_seen.add(name)
        required = {'LICENSES.html', 'runtime/SHA256SUMS', 'runtime/mixer-release.json',
                    'config/mixer-release.json', 'config/firmware-profiles.json'}
        required.update(f'xtask/mixer/{s}' for s in SCRIPTS)
        required.update(f'runtime/{n}' for n in RUNTIME)
        if not required <= names_seen:
            raise ValueError('Incomplete mixer archive')
        for member in members:
            data = archive.extractfile(member).read()
            staged = OUTPUT / member.name
            staged.parent.mkdir(parents=True, exist_ok=True)
            staged.write_bytes(data)


def prepare():
    OUTPUT.mkdir(parents=True, exist_ok=True)
    value = identity()
    version = value['version']
    archive_name, metadata_name = names(version)
    tag = f'mixer-v{version}'
    # Local act dry-runs may lack GitHub credentials; never publish from act.
    info = None if os.environ.get('ACT') else release_info(tag)
    if info is not None:
        if info.get('draft') or info.get('prerelease'):
            raise ValueError(f'{tag} exists but is not a published stable mixer release')
        with tempfile.TemporaryDirectory() as directory:
            subprocess.run(['gh', 'release', 'download', tag, '--repo', os.environ['GH_REPO'],
                            '--pattern', archive_name, '--pattern', metadata_name, '--dir', directory], check=True)
            metadata = json.loads((Path(directory) / metadata_name).read_text())
            validate_archive(Path(directory) / archive_name, metadata)
            for name in (archive_name, metadata_name):
                (OUTPUT / name).write_bytes((Path(directory) / name).read_bytes())
        print(f'Reusing immutable mixer release {tag}')
    else:
        tags = subprocess.check_output(['git', 'tag', '--list', 'mixer-v*'], cwd=ROOT, text=True).splitlines()
        previous = [tuple(map(int, t[7:].split('.'))) for t in tags
                    if re.fullmatch(r'mixer-v[0-9]+\.[0-9]+\.[0-9]+', t)]
        if previous and tuple(map(int, version.split('.'))) <= max(previous):
            raise ValueError('New mixer releases must increase the mixer version')
        subprocess.run(['cargo', 'xtask', 'mixer-build'], cwd=ROOT, check=True)
        subprocess.run(['cargo', 'xtask', 'licenses', str(OUTPUT / 'LICENSES.html')], cwd=ROOT, check=True)
        pack()
        print(f'Built new mixer release {tag}')


def pack():
    value = identity()
    archive_name, metadata_name = names(value['version'])
    files = {'LICENSES.html': (OUTPUT / 'LICENSES.html').read_bytes()}
    for name in RUNTIME:
        files[f'runtime/{name}'] = (ROOT / 'target/mixer' / name).read_bytes()
    for name in SCRIPTS:
        data = (ROOT / 'xtask/mixer' / name).read_bytes().replace(b'\r\n', b'\n')
        if name == 'rc2fx.sh':
            data = data.replace(b'\nrc2fx_build=development\n', b'\nrc2fx_build=release\n')
        files[f'xtask/mixer/{name}'] = data
    for name in ('mixer-release.json', 'firmware-profiles.json'):
        files[f'config/{name}'] = (ROOT / 'config' / name).read_bytes()
    archive_path = OUTPUT / archive_name
    with tarfile.open(archive_path, 'w:gz') as archive:
        for name, data in sorted(files.items()):
            member = tarfile.TarInfo(name)
            member.size = len(data)
            member.mode = 0o644
            archive.addfile(member, io.BytesIO(data))
    metadata = {'identity': value, 'sha256': digest(archive_path.read_bytes()), 'sourceSha256': source_digest()}
    (OUTPUT / metadata_name).write_text(json.dumps(metadata, indent=2) + '\n')
    validate_archive(archive_path, metadata)


def publish():
    value = identity()
    version = value['version']
    tag = f'mixer-v{version}'
    archive_name, metadata_name = names(version)
    metadata = json.loads((OUTPUT / metadata_name).read_text())
    if digest((OUTPUT / archive_name).read_bytes()) != metadata['sha256'] or metadata['identity'] != value:
        raise ValueError('Invalid prepared mixer release')
    existing = release_info(tag)
    if existing is not None:
        if existing.get('draft') or existing.get('prerelease'):
            raise ValueError(f'{tag} is not a published stable release')
        with tempfile.TemporaryDirectory() as directory:
            subprocess.run(['gh', 'release', 'download', tag, '--repo', os.environ['GH_REPO'],
                            '--pattern', metadata_name, '--dir', directory], check=True)
            if json.loads((Path(directory) / metadata_name).read_text()) != metadata:
                raise ValueError('Refusing to replace an existing mixer release')
        return
    app_tag = json.loads((OUTPUT / 'plan.json').read_text())['tag']
    source = subprocess.check_output(['git', 'rev-parse', app_tag], cwd=ROOT, text=True).strip()
    subprocess.run(['gh', 'release', 'create', tag, str(OUTPUT / archive_name), str(OUTPUT / metadata_name),
                    '--repo', os.environ['GH_REPO'], '--target', source, '--latest=false',
                    '--title', f'RC2FX mixer software {version}',
                    '--notes', f'RC2FX mixer software {version}. Bundled by compatible desktop releases. Install through the desktop app.'], check=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['prepare', 'publish'])
    args = parser.parse_args()
    globals()[args.command]()
