#!/bin/sh
# Release CI replaces this placeholder with its trusted Ed25519 public key.
set -eu
release_public_key='@HORDE_RELEASE_PUBLIC_KEY_PEM@'
version=''
service='ask'
repair=no
while [ "$#" -gt 0 ]; do
  case "$1" in
    --version) version=$2; shift 2 ;;
    --service) service=yes; shift ;;
    --no-service) service=no; shift ;;
    --repair) repair=yes; shift ;;
    --help) echo 'Usage: install.sh [--version VERSION] [--service|--no-service] [--repair]'; exit 0 ;;
    *) echo "Unknown option: $1" >&2; exit 1 ;;
  esac
done
case "$release_public_key" in *@HORDE_*) echo 'Download install.sh from an official Horde release.' >&2; exit 1;; esac
for dependency in curl python3 tar; do
  command -v "$dependency" >/dev/null 2>&1 || { echo "Required command missing: $dependency" >&2; exit 1; }
done
case "$(uname -s)" in Darwin) platform=apple-darwin;; Linux) platform=unknown-linux-musl;; *) echo 'Supported systems: macOS, Linux' >&2; exit 1;; esac
case "$(uname -m)" in arm64|aarch64) arch=aarch64;; x86_64) arch=x86_64;; *) echo 'Unsupported architecture' >&2; exit 1;; esac
case "$version" in *[!0-9A-Za-z.-]*) echo 'Invalid version' >&2; exit 1;; esac
base=https://horde.sh/releases/latest
if [ -n "$version" ]; then base="https://horde.sh/releases/v$version"; fi
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT HUP INT TERM
printf '%s\n' "$release_public_key" > "$scratch/key.pem"
curl --fail --silent --show-error --location --proto '=https' "$base/manifest.json" -o "$scratch/manifest.json"
curl --fail --silent --show-error --location --proto '=https' "$base/manifest.sig" -o "$scratch/manifest.sig"
python3 -I - "$scratch/key.pem" "$scratch/manifest.json" "$scratch/manifest.sig" <<'PY'
# Minimal Ed25519 verifier using only Python's standard library. This keeps the
# installer dependency-free on macOS and Linux while retaining signature checks.
import base64, hashlib, pathlib, re, sys
q = 2**255 - 19
l = 2**252 + 27742317777372353535851937790883648493
d = -121665 * pow(121666, q - 2, q) % q
I = pow(2, (q - 1) // 4, q)
def xrecover(y):
    xx = (y*y - 1) * pow(d*y*y + 1, q - 2, q) % q
    x = pow(xx, (q + 3) // 8, q)
    if (x*x - xx) % q: x = x * I % q
    return x if x & 1 == 0 else q - x
def add(P, Q):
    x1,y1 = P; x2,y2 = Q
    z = (1 + d*x1*x2*y1*y2) % q
    return ((x1*y2+x2*y1)*pow(z,q-2,q)%q, (y1*y2+x1*x2)*pow(1-d*x1*x2*y1*y2,q-2,q)%q)
def scalarmult(P, e):
    if e == 0: return (0, 1)
    Q = scalarmult(P, e // 2)
    Q = add(Q, Q)
    return add(Q, P) if e & 1 else Q
def decode(s):
    if len(s) != 32: raise ValueError('bad point')
    y = int.from_bytes(s, 'little') & ((1<<255)-1)
    P = (xrecover(y), y)
    if (P[0] & 1) != (s[31] >> 7): P = (q-P[0], P[1])
    return P
der = base64.b64decode(''.join(x for x in pathlib.Path(sys.argv[1]).read_text().splitlines() if not x.startswith('---')))
key = der[-32:]
message = pathlib.Path(sys.argv[2]).read_bytes()
signature = pathlib.Path(sys.argv[3]).read_bytes()
if len(key) != 32 or len(signature) != 64: raise SystemExit('Invalid release key or signature')
A = decode(key); R = decode(signature[:32]); S = int.from_bytes(signature[32:], 'little')
if S >= l: raise SystemExit('Invalid release signature')
h = int.from_bytes(hashlib.sha512(signature[:32] + key + message).digest(), 'little') % l
B = (xrecover(4 * pow(5, q-2, q) % q), 4 * pow(5, q-2, q) % q)
if scalarmult(B, S) != add(R, scalarmult(A, h)): raise SystemExit('Release signature verification failed')
PY
python3 -I - "$scratch" "$arch-$platform" "$version" <<'PY'
import json, pathlib, re, sys
scratch, target, requested = pathlib.Path(sys.argv[1]), sys.argv[2], sys.argv[3]
m = json.loads((scratch/'manifest.json').read_text())
if not re.fullmatch(r'[0-9][0-9A-Za-z.-]{0,63}', m['version']): raise SystemExit('Invalid release version')
if requested and requested != m['version']: raise SystemExit('Release version mismatch')
if not requested and '-' in m['version']: raise SystemExit('Prerelease requires explicit version')
a = next(a for a in m['artifacts'] if a['target'] == target)
if not a['url'].startswith('https://horde.sh/releases/v'): raise SystemExit('Untrusted artifact URL')
if not re.fullmatch(r'[0-9a-f]{64}', a['sha256']): raise SystemExit('Invalid artifact checksum')
for name, value in [('version',m['version']),('url',a['url']),('hash',a['sha256'])]:
    (scratch/name).write_text(value)
skills = [a for a in m['artifacts'] if a['target'] == 'skills']
if len(skills) > 1: raise SystemExit('Duplicate skills artifact')
if skills:
    a = skills[0]
    if not a['url'].startswith('https://horde.sh/releases/v'): raise SystemExit('Untrusted skills URL')
    if not re.fullmatch(r'[0-9a-f]{64}', a['sha256']): raise SystemExit('Invalid skills checksum')
    (scratch/'skills-url').write_text(a['url'])
    (scratch/'skills-hash').write_text(a['sha256'])
PY
curl --fail --silent --show-error --location --proto '=https' "$(cat "$scratch/url")" -o "$scratch/horde.tar"
if [ -f "$scratch/skills-url" ]; then
  curl --fail --silent --show-error --location --proto '=https' "$(cat "$scratch/skills-url")" -o "$scratch/skills.tar"
fi
python3 -I - "$scratch" <<'PY'
import hashlib, pathlib, sys, tarfile
p = pathlib.Path(sys.argv[1])
def extract(archive, hash_name, skills_only):
    if archive.stat().st_size > (16 if skills_only else 272)*1024*1024: raise SystemExit('Release archive too large')
    if hashlib.sha256(archive.read_bytes()).hexdigest() != (p/hash_name).read_text(): raise SystemExit('Artifact checksum mismatch')
    seen = set(); skill_bytes = 0; found = False
    with tarfile.open(archive, mode='r:') as tar:
        for entry in tar:
            name = entry.name.rstrip('/')
            parts = name.split('/')
            if (len(name) > 1024 or any(part in ('', '.', '..') for part in parts)
                or any(ord(c) < 32 or ord(c) == 127 for c in name) or '\\' in name or ':' in name
                or not ((entry.isfile() and name == 'horde') or (entry.isdir() and name == 'skills') or name.startswith('skills/'))):
                raise SystemExit('Invalid release archive path')
            if not (entry.isfile() or entry.isdir()): raise SystemExit('Invalid release archive entry type')
            if len(seen) >= 4096 or name.lower() in seen: raise SystemExit('Duplicate or excessive release entries')
            seen.add(name.lower())
            target = p/name
            if entry.isdir():
                if entry.size: raise SystemExit('Release directory has content')
                target.mkdir(parents=True, exist_ok=True)
                continue
            if skills_only and name == 'horde': raise SystemExit('Binary in skills artifact')
            if name == 'horde':
                if entry.size > 256*1024*1024: raise SystemExit('Binary too large')
                found = True
            else:
                skill_bytes += entry.size
                if entry.size > 1024*1024 or skill_bytes > 8*1024*1024: raise SystemExit('Release skills too large')
            target.parent.mkdir(parents=True, exist_ok=True)
            data = tar.extractfile(entry).read()
            if len(data) != entry.size: raise SystemExit('Truncated release file')
            with target.open('xb') as output: output.write(data)
            target.chmod(0o755 if name == 'horde' or entry.mode & 0o111 else 0o644)
    if not skills_only and not found: raise SystemExit('Release binary missing')
    if skills_only and not (p/'skills').is_dir(): raise SystemExit('Release skills missing')
extract(p/'horde.tar', 'hash', False)
if (p/'skills.tar').exists(): extract(p/'skills.tar', 'skills-hash', True)
PY
chmod 755 "$scratch/horde"
version=$(cat "$scratch/version")
[ "$("$scratch/horde" --version)" = "horde $version" ]
install_root="$HOME/.local/share/horde-install"
if [ "$repair" = yes ]; then
  [ -e "$install_root/current/horde" ] || { echo 'Repair requires an existing managed installation.' >&2; exit 1; }
  # Run the verified new updater so old schema bugs cannot block its own fix.
  # It retains normal update locking, migration backups and restart checks.
  "$scratch/horde" update --version "$version"
  exit $?
fi
mkdir -p "$install_root" "$HOME/.local/bin"
# Shared lock format with the native updater is held by Python during installation.
python3 -I - "$scratch/horde" "$install_root" "$version" <<'PY'
import fcntl, os, pathlib, shutil, sys, uuid
binary, root, version = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2]), sys.argv[3]
with (root/'update.lock').open('a') as lock:
    fcntl.flock(lock, fcntl.LOCK_EX|fcntl.LOCK_NB)
    if (root/'current').exists():
        raise SystemExit('Horde is already installed; use horde update to drain and update safely.')
    release=root/(version+'-'+str(uuid.uuid4())); release.mkdir()
    shutil.copy2(binary,release/'horde')
    if (binary.parent/'skills').is_dir(): shutil.copytree(binary.parent/'skills', release/'skills')
    temporary=root/('next-'+str(uuid.uuid4())); temporary.symlink_to(release)
    os.replace(temporary,root/'current')
PY
launcher="$HOME/.local/bin/horde"
cat > "$launcher" <<'LAUNCHER'
#!/bin/sh
export HORDE_LAUNCHER="$HOME/.local/bin/horde"
exec "$HOME/.local/share/horde-install/current/horde" "$@"
LAUNCHER
chmod 755 "$HOME/.local/bin/horde"
if [ -d /usr/local/bin ] && [ -w /usr/local/bin ]; then
  ln -sf "$HOME/.local/bin/horde" /usr/local/bin/horde
  launcher=/usr/local/bin/horde
elif [ -d "$HOME/.local/bin" ]; then
  shell_name=$(basename "${SHELL:-sh}")
  case "$shell_name" in
    zsh) shell_rc="$HOME/.zprofile" ;;
    bash) shell_rc="$HOME/.bash_profile" ;;
    fish) shell_rc="$HOME/.config/fish/config.fish" ;;
    *) shell_rc="$HOME/.profile" ;;
  esac
  mkdir -p "$(dirname "$shell_rc")"
  touch "$shell_rc"
  if ! grep -Fq '$HOME/.local/bin' "$shell_rc" 2>/dev/null; then
    printf '\n# Horde\nexport PATH="$HOME/.local/bin:$PATH"\n' >> "$shell_rc"
  fi
fi
# Prompts on the controlling terminal, so it still asks under `curl ... | sh`, and
# falls back to printing the command to run when there is no terminal at all.
"$HOME/.local/bin/horde" config init --interactive
if [ "$service" = ask ]; then
  service=no
  if [ -t 0 ]; then
    printf 'Start Horde at machine boot as your user? [y/N] '
    read -r answer
    case "$answer" in y|Y|yes|YES) service=yes;; esac
  elif [ -r /dev/tty ] && [ -w /dev/tty ]; then
    if printf 'Start Horde at machine boot as your user? [y/N] ' > /dev/tty 2>/dev/null; then
      read -r answer < /dev/tty || answer=no
      case "$answer" in y|Y|yes|YES) service=yes;; esac
    fi
  fi
fi
if [ "$service" = yes ]; then "$HOME/.local/bin/horde" service install; fi
printf 'Installed Horde %s. Open a new terminal, then run: horde --version\n' "$version"
