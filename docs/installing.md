# Installation and releases

Official releases provide `install.sh`, signed `manifest.json`/`manifest.sig`,
`SHA256SUMS`, uncompressed binary tar archives, and a multi-architecture OCI image.
Targets are macOS ARM64/x86-64 and Linux ARM64/x86-64 (static musl binaries).

```sh
curl -fsSL https://horde.sh/install -o install.sh
sh install.sh
# For automation, choose explicitly:
sh install.sh --version 0.3.0 --no-service
```

The installer downloaded from a release verifies the signed release before it
installs anything. The canonical template is
`website/scripts/install-template.sh`; the root `install.sh` runs it for source
checkouts. The unrendered template cannot install unsigned builds. Existing
managed installations use `horde update`.

Interactive installation asks whether Horde should start at machine boot.
Without a terminal it installs only the binary unless `--service` is supplied.
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
four binary builds run concurrently. Publication waits for every check and build
to pass, then publishes digest-addressed images, the signed manifest, and the
installer. Tags containing a prerelease suffix are marked prerelease; automatic
updates use the latest stable release.

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

After merging the release changes, publish the version in `Cargo.toml`:

```sh
git switch main
git pull --ff-only
git tag v0.3.0
git push origin v0.3.0
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
