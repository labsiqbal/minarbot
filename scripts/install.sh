#!/bin/sh
set -eu
umask 077

command -v python3 >/dev/null 2>&1 || { echo 'Python 3 is required.' >&2; exit 1; }
exec python3 - "$@" 3<&0 <<'PY'
import argparse
import base64
import contextlib
import fcntl
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import plistlib
import re
import shutil
import stat
import subprocess
import tempfile
import time
from urllib.parse import urlsplit
from uuid import uuid4

PUBLIC_KEY = 'kSIv+agT4RsABust/LmvPVjfWDxZxM0YKL03gts/UEM='
RELEASES = 'https://github.com/labsiqbal/minarbot/releases'
LIMIT = 1 << 30
VERSION = r'[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.-]+)?'
ID = VERSION + r'-[a-f0-9]{8,40}'


def require(condition, message):
    if not condition:
        raise ValueError(message)


def curl(url, *, output=None, limit=131072, body=None, origin=None, token=None, redirects=False):
    scheme = '=http,https' if urlsplit(url).scheme == 'http' else '=https'
    command = ['curl', '-qfsS', '--connect-timeout', '5', '--max-time', '300' if output else '15',
               '--proto', scheme, '--max-filesize', str(limit)]
    if redirects:
        command += ['--location', '--max-redirs', '4', '--proto-redir', scheme]
    if output is not None:
        command += ['--output', str(output)]
    input_bytes = None
    if body is not None:
        command += ['-H', 'Content-Type: application/json', '--data-binary', '@-' if token is None else json.dumps(body)]
        if token is None:
            input_bytes = json.dumps(body).encode()
    if origin is not None:
        command += ['-H', 'Origin: ' + origin]
    if token is not None:
        command += ['-H', '@-']
        input_bytes = ('Authorization: Bearer ' + token + '\n').encode()
    result = subprocess.run(command + ['--', url], input=input_bytes, stdout=subprocess.PIPE,
                            stderr=subprocess.DEVNULL, check=False)
    require(result.returncode == 0, 'Request failed: ' + url)
    return result.stdout


def valid_origin(value):
    url = urlsplit(value)
    return (url.netloc and not url.username and not url.password and url.hostname
            and value == f'{url.scheme}://{url.netloc}'
            and (url.scheme == 'https' or
                 url.scheme == 'http' and url.hostname in ('127.0.0.1', '::1'))
            and not any(c.isspace() or ord(c) < 32 for c in value))


def openssl():
    candidates = ['/opt/homebrew/opt/openssl@3/bin/openssl',
                  '/usr/local/opt/openssl@3/bin/openssl', shutil.which('openssl')]
    for executable in filter(None, candidates):
        if os.access(executable, os.X_OK):
            version = subprocess.check_output([executable, 'version'], text=True)
            if re.match(r'OpenSSL [3-9]\.', version):
                return executable
    raise ValueError('OpenSSL 3 is required. On macOS, run brew install openssl@3.')


def signed_archive(stage, base, system, arch, tag, embedded):
    prefix = f'download/{tag}' if tag else 'latest/download'
    manifest_path = stage / 'latest.json'
    signature = stage / 'signature'
    key = stage / 'key.der'
    manifest_path.write_bytes(curl(f'{base}/{prefix}/latest.json', redirects=True))
    signature.write_bytes(base64.b64decode(
        curl(f'{base}/{prefix}/latest.json.sig', limit=88, redirects=True), validate=True))
    key.write_bytes(bytes.fromhex('302a300506032b6570032100') + base64.b64decode(PUBLIC_KEY))
    verified = subprocess.run(
        [openssl(), 'pkeyutl', '-verify', '-pubin', '-keyform', 'DER', '-inkey', key,
         '-rawin', '-in', manifest_path, '-sigfile', signature],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
    require(verified.returncode == 0, 'Invalid GitHub release signature.')
    manifest = json.loads(manifest_path.read_bytes())
    version, release_id, commit = (manifest.get(k, '') for k in ('version', 'id', 'commit'))
    require(re.fullmatch(VERSION, version) and re.fullmatch(ID, release_id)
            and re.fullmatch(r'[a-f0-9]{40}', commit) and release_id.startswith(version + '-')
            and commit.startswith(release_id[len(version) + 1:])
            and (tag is None or tag == f'v{version}'), 'Invalid GitHub release metadata.')
    name = f'minarbot-{version}-{system}-{arch}.tar.gz'
    asset = manifest.get('assets', {}).get(name)
    if asset is None and system == 'linux' and arch == 'x86_64' and manifest.get('archive') == name:
        asset = manifest
    require(asset is not None, f'Release does not contain {name}.')
    digest, size = asset.get('sha256', ''), asset.get('size', 0)
    require(re.fullmatch(r'[a-f0-9]{64}', digest) and type(size) is int and 0 < size <= LIMIT,
            'Invalid GitHub release archive metadata.')
    archive = stage / name
    if embedded:
        fd = os.open(embedded, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(fd, 'rb') as source, archive.open('xb') as output:
            info = os.fstat(source.fileno())
            require(stat.S_ISREG(info.st_mode) and info.st_size == size, 'Release archive size mismatch.')
            remaining = size
            while remaining:
                chunk = source.read(min(65536, remaining))
                require(chunk, 'Release archive size mismatch.')
                output.write(chunk)
                remaining -= len(chunk)
    else:
        curl(f'{base}/download/v{version}/{name}', output=archive, limit=size, redirects=True)
    require(archive.stat().st_size == size, 'Release archive size mismatch.')
    with archive.open('rb') as stream:
        checksum = hashlib.sha256()
        for chunk in iter(lambda: stream.read(65536), b''):
            checksum.update(chunk)
    require(checksum.hexdigest() == digest, 'Release archive SHA-256 mismatch.')
    return archive, manifest


def extract(archive, destination, manifest):
    import tarfile
    destination.mkdir(mode=0o700)
    total = 0
    with tarfile.open(archive, 'r|gz') as package:
        for member in package:
            relative = PurePosixPath(member.name)
            require(not relative.is_absolute() and '..' not in relative.parts
                    and (member.isdir() or member.isfile()), 'Unsafe archive entry.')
            target = destination.joinpath(*relative.parts)
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
            else:
                total += member.size
                require(total <= 2 * LIMIT and target != destination, 'Release exceeds extraction bounds.')
                target.parent.mkdir(parents=True, exist_ok=True)
                with package.extractfile(member) as source, target.open('xb') as output:
                    shutil.copyfileobj(source, output)
                if member.mode & 0o111:
                    target.chmod(0o700)
    info = json.loads((destination / 'release.json').read_bytes())
    require(all(info.get(k) == manifest[k] for k in ('id', 'version', 'commit')),
            'Archive release metadata does not match the signed manifest.')
    require(all(os.access(destination / 'bin' / binary, os.X_OK) for binary in
                ('minarbot-server', 'minarbot-launcher')), 'Release lacks Rust binaries.')


def write_private(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as stream:
        temp = Path(stream.name)
        try:
            stream.write(value)
            stream.flush()
            os.fsync(stream.fileno())
            temp.replace(path)
        finally:
            temp.unlink(missing_ok=True)


def private_file(path):
    info = path.lstat()
    require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid()
            and info.st_nlink == 1 and info.st_mode & 0o077 == 0,
            'Launcher configuration must be a private file owned by this user.')


def local_install(root, config, args, system, arch, base):
    releases, data = root / 'releases', root / 'data'
    for directory in (releases, data, config.parent):
        directory.mkdir(parents=True, exist_ok=True)
    fd = os.open(releases / '.install.lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd) as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        current = releases / 'current'
        if current.is_symlink():
            installed = current.resolve(strict=True)
            release_id = installed.name
            require(re.fullmatch(ID, release_id) and installed.parent == releases.resolve()
                    and installed.is_dir(),
                    'Invalid existing Minarbot release selection.')
        else:
            require(not current.exists(), 'Existing current path must be a symlink.')
            with tempfile.TemporaryDirectory(prefix='.install-', dir=releases) as temporary:
                stage = Path(temporary)
                archive, manifest = signed_archive(stage, base, system, arch, args.tag, args.archive)
                release_id = manifest['id']
                extract(archive, stage / 'release', manifest)
                destination = releases / release_id
                require(not destination.is_symlink(), 'Invalid existing Minarbot release directory.')
                if not destination.exists():
                    (stage / 'release').rename(destination)
                else:
                    require((destination / 'release.json').read_bytes() ==
                            (stage / 'release/release.json').read_bytes(),
                            'Existing release metadata differs from the signed release.')
                (stage / 'current').symlink_to(release_id)
                (stage / 'current').replace(current)
        env = config.parent / 'launcher.env'
        if not env.exists():
            require(not env.is_symlink(), 'Invalid launcher.env symlink.')
            values = '# Optional literal environment values for Minarbot.\n'
            if args.public_origin:
                values += 'MINARBOT_ALLOWED_ORIGINS=' + args.public_origin + '\n'
            write_private(env, values.encode())
        private_file(env)
        if config.exists():
            private_file(config)
            settings = json.loads(config.read_bytes())
            require(isinstance(settings, dict), 'Invalid launcher.json.')
            port = settings.get('port')
            port = 4317 if port is None else port
            require(type(port) in (int, float) and 1 <= port <= 65535 and port == int(port)
                    and args.port in (None, port),
                    'Existing launcher.json uses a different port.')
            port = int(port)
        else:
            require(not config.is_symlink(), 'Invalid launcher.json symlink.')
            port = args.port or 4317
            settings = dict(dataDir=str(data), releasesDir=str(releases), envFile=str(env), port=port)
            write_private(config, (json.dumps(settings) + '\n').encode())
        return release_id, port, [str(current / 'bin/minarbot-launcher'), '--config', str(config)]


UNIT = b'''[Unit]
Description=Minarbot Rust server launcher
After=network.target

[Service]
Type=simple
ExecStart="%h/.local/share/minarbot/releases/current/bin/minarbot-launcher" --config "%h/.config/minarbot/launcher.json"
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
'''


def service(home, root, system, command):
    if system == 'linux':
        write_private(home / '.config/systemd/user/minarbot.service', UNIT)
        subprocess.run(['systemctl', '--user', 'daemon-reload'], check=True)
        subprocess.run(['systemctl', '--user', 'enable', '--now', 'minarbot.service'], check=True)
    else:
        label = 'com.minarbot.server'
        agent = home / 'Library/LaunchAgents' / (label + '.plist')
        logfile = str(root / 'server.log')
        write_private(agent, plistlib.dumps(dict(Label=label, ProgramArguments=command,
                       EnvironmentVariables={'HOME': str(home),
                                             'PATH': '/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin'},
                       RunAtLoad=True, KeepAlive={'SuccessfulExit': False},
                       StandardOutPath=logfile, StandardErrorPath=logfile)))
        domain = f'gui/{os.getuid()}'
        if subprocess.run(['launchctl', 'print', domain], stdout=subprocess.DEVNULL,
                          stderr=subprocess.DEVNULL).returncode:
            domain = f'user/{os.getuid()}'
        # A healthy existing service owns the same config and should keep running.
        if subprocess.run(['launchctl', 'print', f'{domain}/{label}'], stdout=subprocess.DEVNULL,
                          stderr=subprocess.DEVNULL).returncode:
            subprocess.run(['launchctl', 'enable', f'{domain}/{label}'], check=True)
            subprocess.run(['launchctl', 'bootstrap', domain, str(agent)], check=True)


def healthy(local, release_id):
    try:
        info = json.loads(curl(local + '/api/environment'))
        return info.get('version') == release_id.rsplit('-', 1)[0] and info.get('id')
    except ValueError:
        return False


def wait_for_ui(local, release_id, child):
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        require(child is None or child.poll() is None, 'Minarbot launcher exited during startup.')
        if healthy(local, release_id):
            return
        time.sleep(.5)
    raise ValueError('Minarbot did not become healthy. Check the user service logs.')


def pair(local, args):
    machines = json.loads(curl(local + '/api/machines'))
    for machine in machines.get('machines', []):
        if machine.get('origin') == args.source:
            require(machine.get('pairingState') == 'paired', 'Unpair the incomplete connection before retrying.')
            return
    environment = json.loads(curl(local + '/api/environment'))
    require(urlsplit(args.source).scheme != 'https' or urlsplit(environment['origin']).scheme == 'https',
            "Set this machine's reachable HTTPS address in MINARBOT_ALLOWED_ORIGINS before pairing.")
    require(not args.public_origin or args.public_origin == environment['origin'],
            'The running Minarbot uses a different HTTPS origin. Check launcher.env before pairing.')
    name = args.name or platform.node()
    curl(local + '/api/environment', body={'id': str(uuid4()), 'name': name}, origin=local)
    reply = json.loads(curl(args.source + '/api/install/pairing-code', body={}, token=args.token))
    code = reply.get('code', '')
    require(re.fullmatch(r'[A-Z0-9]{12}', code), 'Source returned an invalid pairing code.')
    curl(local + '/api/machines', body={'id': str(uuid4()), 'origin': args.source, 'code': code},
         origin=local)


def main():
    parser = argparse.ArgumentParser(description='Install Minarbot and optionally pair with another machine.')
    parser.add_argument('--source')
    parser.add_argument('--token')
    parser.add_argument('--token-stdin', action='store_true', help=argparse.SUPPRESS)
    parser.add_argument('--name')
    parser.add_argument('--port', type=int)
    parser.add_argument('--public-origin')
    parser.add_argument('--no-open', action='store_true')
    parser.add_argument('--serve', action='store_true', help=argparse.SUPPRESS)  # Legacy copied commands.
    parser.add_argument('--wsl', action='store_true', help=argparse.SUPPRESS)
    parser.add_argument('--tag', help=argparse.SUPPRESS)
    parser.add_argument('--archive', type=Path, help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.token_stdin:
        require(args.token is None, 'Choose one install token input.')
        with os.fdopen(3, encoding='ascii') as stream:
            args.token = stream.read(49).rstrip('\n')
    else:
        os.close(3)
    require(bool(args.source) == bool(args.token), 'Pass both --source and --token to pair.')
    if args.source:
        require(valid_origin(args.source), 'Source must be an exact HTTPS origin (HTTP only on loopback).')
        require(re.fullmatch(r'bsi_[A-Za-z0-9_-]{43}', args.token), 'Install token is invalid.')
    require(not args.public_origin or valid_origin(args.public_origin)
            and args.public_origin.startswith('https://') and ',' not in args.public_origin,
            'Public origin must be an exact HTTPS origin.')
    require(args.port is None or 1 <= args.port <= 65535, 'Invalid port.')
    require(args.tag is None or re.fullmatch('v' + VERSION, args.tag), 'Invalid release tag.')
    require(args.archive is None or (args.tag and args.wsl), 'Embedded archive requires WSL and a release tag.')
    home = Path(os.environ.get('HOME', ''))
    require(home.is_absolute(), 'HOME must be absolute.')
    system = {'Linux': 'linux', 'Darwin': 'macos'}.get(platform.system())
    arch = {'x86_64': 'x86_64', 'arm64': 'aarch64', 'aarch64': 'aarch64'}.get(platform.machine())
    require(system and arch and (not args.wsl or system == 'linux'), 'Unsupported installation platform.')
    require(shutil.which('curl'), 'curl is required.')
    base = os.environ.get('MINARBOT_RELEASE_BASE', RELEASES).rstrip('/')
    if base != RELEASES:
        fixture = urlsplit(base)
        require(fixture.scheme == 'http' and fixture.hostname in ('127.0.0.1', '::1')
                and not fixture.username and not fixture.password and not fixture.query
                and not fixture.fragment, 'Release base override must use loopback HTTP.')
    root, config = home / '.local/share/minarbot', home / '.config/minarbot/launcher.json'
    release_id, port, command = local_install(root, config, args, system, arch, base)
    local = f'http://127.0.0.1:{port}'
    child = None
    try:
        if args.wsl:
            if not healthy(local, release_id):
                child = subprocess.Popen(command)
        else:
            service(home, root, system, command)
        wait_for_ui(local, release_id, child)
        if args.source:
            pair(local, args)
        print(f'Minarbot {release_id} is ready at {local}', flush=True)
        opener = 'open' if system == 'macos' else 'xdg-open'
        if not args.no_open and not args.wsl and shutil.which(opener):
            with contextlib.suppress(OSError):
                subprocess.Popen([opener, local], stdin=subprocess.DEVNULL,
                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if child is not None:
            raise SystemExit(child.wait())
    finally:
        if child is not None and child.poll() is None:
            child.terminate()
            child.wait()


try:
    main()
except (ValueError, OSError, subprocess.CalledProcessError, TypeError, KeyError, RuntimeError) as error:
    raise SystemExit(f'Minarbot install failed. {error}') from None
PY
