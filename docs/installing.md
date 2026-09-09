# Installation and releases

Official releases provide `install.sh`, signed `manifest.json`/`manifest.sig`,
`SHA256SUMS`, uncompressed binary tar archives, a separate `skills.tar`, and a
multi-architecture OCI image.
Targets are macOS ARM64/x86-64 and Linux ARM64/x86-64 (static musl binaries).

```sh
curl -fsSL https://horde.sh/install -o install.sh
sh install.sh
# For automation, choose explicitly:
sh install.sh --version VERSION --no-service
```

Replace `VERSION` with a published release version.

The installer downloaded from a release verifies the signed release before it
installs anything. The canonical template is
`website/scripts/install-template.sh`; the root `install.sh` runs it for source
checkouts. The unrendered template cannot install unsigned builds. Existing
managed installations use `horde update`.

Interactive installation asks whether Horde should start at machine boot.
Without a terminal it installs the binary and skills without a boot service unless
`--service` is supplied.
Linux installs a systemd system service; macOS installs a LaunchDaemon. Both run
as the installing user, including their HOME and a configured tool PATH. Only
service setup/removal invokes sudo. Provider credentials are not embedded in
world-readable service definitions.

```sh
horde service install
horde service status
horde service uninstall
```

The stable launcher is `horde`. The installer links it into `/usr/local/bin`
when that directory is writable; otherwise it adds `~/.local/bin` to the
startup file for your current shell and tells you to open a new terminal.
Versioned executables and the `current` link live under
`~/.local/share/horde-install`. Runtime databases and workspaces remain in the
configured data directory. Service uninstall retains those files and credentials.

## Make Horde the default in a repository

Run one of these commands from the repository you want to work on:

```sh
horde init --agent codex --delegate always
horde init --agent claude --delegate always
```

Choose the agent you talk to. This choice does not change the providers assigned
to Horde's workers. Use `--repo /absolute/path` to select another repository, or
`horde --data-dir /absolute/path init --agent codex --delegate always` to connect
to a specific Horde instance. An explicit `--data-dir` or `HORDE_DATA_DIR` is
pinned in the generated MCP arguments.

The repository needs an initial commit and a configured Git author identity.
The binary bundles the repository skills and their resources, including the
operator, worker, and template skills plus the workflow guidance for setup,
discovery, model selection, planning, delegation, and review. Installing these
repository files does not need npm or a download. The daemon also needs its
[default runtime skill pack](#skill-files-and-older-updaters), which official
installers place beside the executable.

| Agent | Instructions | Skills | MCP configuration |
| --- | --- | --- | --- |
| Codex | `AGENTS.md` | `.agents/skills/` | `.codex/config.toml` |
| Claude Code | `CLAUDE.md` | `.claude/skills/` | `.mcp.json` |

The managed instruction block directs the agent to delegate repository changes,
including small edits, to Horde. The agent keeps responsibility for the user
conversation and result inspection. Workers already executing Horde assignments
perform their assigned steps instead of submitting new root tasks. An explicit
user request can override delegation; instructions do not disable agent tools.

Rerunning `init` updates the managed block and bundled skills. It preserves
unrelated instructions and MCP settings, and refuses conflicting MCP entries,
malformed managed blocks, symlink destinations, or locally modified bundled
skills. Resolve the reported conflict and rerun. Skill ownership hashes are stored
in the selected agent's configuration directory so upgrades can distinguish
installed content from local edits. Files are replaced atomically one at a time;
initialization is not a transaction across the repository.

Initialization checks the configured workflow and required executables and
credentials locally. It also runs a simulated task in a temporary repository with
an isolated daemon, without provider calls. That test verifies scheduling and task
completion; it does not verify model authentication or implementation quality.
The temporary daemon and its data are removed after the check.

The JSON report includes `runtime_skills`. A missing or invalid default runtime
pack blocks readiness. If prerequisites are missing, the repository files are
still installed, and the report has `ready: false` with concrete `next_steps`.
Complete those steps and rerun the same command. When local checks pass, `init`
starts or reuses the selected Horde daemon. Authentication remains unprobed.
The generated MCP entry uses `horde` from PATH; your agent must be able to find
the same installation. Reload your agent and accept its repository trust or MCP
prompt if required.

`npx skills add gethamster/horde` is a separate, skills-only installation path.
It does not run `horde init`, configure MCP, or write the always-delegate policy.
Use `horde init` alone when you want the complete repository setup.

## Skill files and older updaters

The signed manifest lists `skills.tar` as an additional artifact. Binary archives
retain their single `horde` entry so older updaters can install the new executable.
Current installers and updaters verify the skill artifact and place its files
beside that executable; container images include the same directories.

When an older updater installs only the new executable, the daemon retrieves the
missing default pack from that exact version's signed release. This runs in the
background and retries download failures while management and existing pinned work
remain available. It preserves any explicitly installed pack, including a pack
installed while the download is in progress. An invalid existing pack reports an
error for inspection instead of being silently replaced.

Later skill edits use `horde skills install DIRECTORY` and worker synchronization;
they do not require another executable release. See [runtime skills](runtime-skills.md).

## Building from source

A source build needs the repository's `skills` directory beside the executable.
For a release build run:

```sh
cargo build --release
cp -R skills target/release/
./target/release/horde doctor
./target/release/horde start
```

When copying the binary elsewhere, copy `skills` to the same destination directory.
An explicitly installed runtime pack takes precedence over the adjacent directory.
Development binaries running under the checkout's `target` directory can use the
repository files directly; copied binaries and release builds cannot rely on that.

`start` and every `doctor` mode check the default pack with the same resolver used
by submission. Their JSON output includes its hash, names, selected path, and lookup
locations. A missing or invalid pack fails these commands with the lookup locations
before starting a new daemon or probing a provider. An invalid installed pack never
falls back silently to adjacent files. Template validation checks structure only;
it does not establish that a runtime has a usable pack.

The foreground `daemon` command prints a startup warning when its pack is unavailable
and keeps management, recovery of pinned work, and signed-release bootstrap available.
Source builds without a release verification key need the local files above.

## Recovering an older database

Some older installations stored workflow steps in `tasks` and their objectives
in `outcomes`, while reporting schema version 2. Their updater can fail with
`no such column: objective` before it downloads the fix. For an existing managed
installation, stop the daemon and run the signed installer's repair mode:

```sh
horde stop
curl -fsSL https://horde.sh/install | sh -s -- --repair
horde start
```

Repair verifies the release and runs the downloaded binary's updater, preserving
the normal update lock and restart checks. The migration saves
`pre-horde-rename-<id>.sqlite3` in the data directory before changing tables.
It preserves stored records and holds unfinished work as blocked for inspection;
it does not automatically replay work pinned to older provider contracts.

Automatic migration requires the original schema 2 outcome trees. Databases with
incomplete trees, federation records, or conflicting populated replacement tables
are refused without changing their records. Keep the database for manual recovery
if repair reports one of these conditions. Package-manager and source installations
must use their owning installer.

## Release operator setup

Configure GitHub Actions with:

- Repository variable `HORDE_RELEASE_PUBLIC_KEY`: the 32-byte Ed25519 public
  key in hex. Build jobs need it without access to the signing environment.
- A `release` environment containing secret `HORDE_RELEASE_PRIVATE_KEY`:
  its PEM private signing key. Only the publish job uses this environment.
- The same environment contains `HORDE_ARTIFACT_DEPLOY_KEY`, a write deploy key
  scoped solely to the public artifact mirror configured in `.github/workflows/release.yml`. It delivers signed files under
  a version tag; that repository verifies signatures and checksums before
  publishing public release assets. It has no access to the source repository.

Keep private signing material out of the repository. The workflow verifies that
the signing key matches the key embedded in binaries, and that the pushed `v*`
tag matches the package version. Formatting, platform lint and test jobs, and the
four binary builds run concurrently. Each Linux runner then builds its container
image on its native architecture while other platform checks continue. These
intermediate images have immutable digest references, with separate layer caches
for AMD64 and ARM64; they receive no release tag yet. Publication waits for every
check and build to pass, verifies both image platforms, combines their digests
under the version tag, and signs the manifest with that combined image digest.
The signed manifest and installer then pass through public download verification.
Tags containing a prerelease suffix are marked prerelease; automatic updates use
the latest stable release.

CI runs on pull requests and pushes to `main`, avoiding duplicate push runs for
PR branches. New commits cancel superseded CI runs; release runs are not cancelled.
Only `main` writes dependency caches, which PR and release jobs can restore.

No release is published just by building this repository. Source builds made
without `HORDE_RELEASE_PUBLIC_KEY` report that they lack an official update
verification key, even after a release has been published. Publishing requires
the signing configuration and a version tag.

## Public hosting and publishing

The Vercel configuration serves the static website, `/release-key.pem`, and
`/release-key.hex` at `https://horde.sh`. `/install` redirects to the latest published
release’s `install.sh`, keeping website deployment independent of binary releases. Its `/releases/latest/<file>` and
`/releases/v<version>/<file>` routes redirect to GitHub Release assets. Release
archives, signatures, checksums, and manifests are stored in the public
artifact mirror reached through `https://horde.sh/releases/latest/manifest.json`;
source is public at `gethamster/horde`. No binary
archives need to be committed or included in a Vercel build. Deploy the website
and assign the domain before relying on these URLs; configuring routes alone
does not make them live.

The website build verifies that the committed PEM and hex keys agree. Release CI
embeds the PEM in its versioned installer and verifies that the repository public-key
variable matches that same committed key. Existing binaries verify updates with
their embedded key; they never adopt a new key merely because a website serves it.

After merging the release changes, replace `VERSION` below with the version in
`Cargo.toml`:

```sh
git switch main
git pull --ff-only
git tag vVERSION
git push origin vVERSION
```

Wait for the source repository’s Release workflow's four platform builds, container publication,
signing, and delivery to the public artifact mirror. Then wait for that public
repository's verification and release workflow to publish the assets. The website
routes resolve the new latest stable release only after public publication.

The workflow's final `verify` job does that check for you: it polls
`https://horde.sh/releases/latest/manifest.json` until it reports the tagged
version, installs that release the way a user would, runs `horde --version`, and
confirms the Linux binary needs no dynamic loader. A red `verify` job means the
tag exists but the release is not usable yet; do not ask remote runtimes to
update until it is green.

The website advertises the latest **published** release, not the version in
`Cargo.toml`. Its build reads the published manifest, warns when the source tree
is ahead, and fails outright if the manifest cannot be read, so horde.sh cannot
name a version that nobody can install.

Keeping the site in step with releases is automatic. `/install` and
`/releases/latest/*` redirect to GitHub's latest release, so they follow a
publication with no rebuild. The generated agent surface
(`/.well-known/mcp.json`, `/.well-known/tools.json`, `llms.txt`) bakes the
version at build time, so the release workflow's `deploy-website` job requests a
production rebuild once `verify` passes and then waits until the site reports
the new version. The Website workflow also reconciles daily: if what horde.sh
advertises falls behind what is published, it rebuilds. Prerelease tags are skipped, because the site
tracks the latest stable release.

This needs one secret, set once. In the Vercel project, create a deploy hook for
the production branch (Settings → Git → Deploy Hooks) and save its URL as the
repository secret `HORDE_WEBSITE_DEPLOY_HOOK`. Without it a release still
publishes and verifies, but `deploy-website` fails to say the site was not
synced.
