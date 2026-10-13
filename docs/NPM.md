# Publishing Lume to npm

How Lume's npm packages get published, how to set up a new one, and what to do when a publish fails.

## The packages

Nine packages, all owned by the `deepbluedynamics` npm account.

**Signal K plugin**: install with `npm install signalk-lume-ti`, or from the Signal K App Store.

| Package | What's in it |
|---|---|
| `signalk-lume-ti` | The plugin, about 85 KB, no program |
| `signalk-lume-ti-linux-arm64` | The `lume` program for a Raspberry Pi |
| `signalk-lume-ti-linux-x64` | The `lume` program for a Linux PC |

**lume command-line tool**: install with `npm install -g @deepbluedynamics/lume`.

| Package | What's in it |
|---|---|
| `@deepbluedynamics/lume` | A 2 KB launcher that provides the `lume` command |
| `@deepbluedynamics/lume-linux-x64` | Linux x64 binary |
| `@deepbluedynamics/lume-linux-arm64` | Linux arm64 binary |
| `@deepbluedynamics/lume-darwin-arm64` | macOS Apple Silicon binary |
| `@deepbluedynamics/lume-darwin-x64` | macOS Intel binary |
| `@deepbluedynamics/lume-win32-x64` | Windows x64 binary |

The launcher packages list the binary packages as *optional dependencies*, each marked with its operating system and CPU. npm downloads only the one that matches the machine. A Pi gets about 35 MB, not every binary.

The HaLOS `marine-lume` .deb doesn't use npm at all. It puts the arm64 program inside the plugin folder, and the plugin looks there first.

## How a normal release publishes

Nobody publishes by hand once a package is set up.

1. Merge the PRs into `main`.
2. In GitHub, go to **Actions → Bump version and release → Run workflow**, keep `patch`, and run it. Or from a terminal:
   ```
   gh workflow run bump-version.yml -R DeepBlueDynamics/lume --ref main -f bump=patch
   ```
3. That workflow bumps the version everywhere, tags it, and runs `release.yml`, which:
   - builds `lume` for all five platforms;
   - builds the HaLOS .deb and the npm tarballs;
   - publishes the GitHub release;
   - runs the **Publish to npm** job. It publishes in this order: the two `signalk-lume-ti-linux-*` packages, the five `@deepbluedynamics/lume-*` binaries, `@deepbluedynamics/lume`, then `signalk-lume-ti`. Binaries go first so the launcher's dependencies already exist when it appears.

The npm job logs in with **trusted publishing**: GitHub proves to npm which repo and workflow is running. No npm token is stored anywhere. Each package on npmjs.com must have a trusted publisher entry that matches **exactly**, or npm refuses the publish.

A release takes about 25–30 minutes, mostly builds.

## Setting up a new package (one time per package)

npm only lets you add a trusted publisher to a package that already exists. So every new package needs **one** hand publish first, then the trusted publisher entry.

### Step 1: publish it once by hand

This has to run in a real terminal window, because npm needs you to approve it in the browser.

```powershell
cd <folder with the .tgz files>
npm whoami                      # must print: deepbluedynamics
npm publish ".\<file>.tgz" --access public
```

When npm prints **"Authenticate your account at: … Press ENTER to open in the browser..."**:

1. Click into that terminal and press **Enter**. The approval page opens in your browser.
2. Approve it.
3. Wait for `+ <package>@<version>`, then "Your package is being processed".

You have about **5 minutes** to press Enter and approve. If you miss it, npm fails with `E404 … /-/v1/done?authId=…` and nothing is published. Run the same command again.

After a first publish, npm shows a `0.0.0-stage` placeholder version for a few minutes while it processes the real one. That's normal.

`npm publish` is close to permanent: you can only unpublish within 72 hours, and a version number can never be reused.

### Step 2: add the trusted publisher

On npmjs.com, open the package, go to **Settings → Trusted Publisher → GitHub Actions**, and fill in:

| Field | Value |
|---|---|
| Organization or user | `DeepBlueDynamics`, **capitalized exactly like this** |
| Repository | `lume` |
| Workflow filename | `bump-version.yml` |
| Environment name | leave blank |
| Allow `npm publish` | **ticked** |
| Allow `npm dist-tag` | leave unticked |

Click **Set up connection**.

- The fields can't be edited after saving. To fix a mistake, **Delete** the entry and add it again.
- The entry shows **"Pending validation"** until a release publishes through it. It must do that within **2 days**, or the entry expires and has to be re-added.
- Optional: add a second entry with workflow `release.yml`. That's only needed if someone runs `release.yml` directly (a tag push or a manual run) instead of going through bump-version.

Why `bump-version.yml` and not `release.yml`: `release.yml` runs *inside* `bump-version.yml`, and npm checks the outer workflow.

Why the capitalization matters: GitHub identifies the repo as `DeepBlueDynamics/lume`. An entry saved as `deepbluedynamics/lume` is treated as a different repo, and the publish fails with `E404 Not Found - PUT https://registry.npmjs.org/<package>`.

## Adding a new package to the release

1. Make the package build in the release: a packer in `scripts/`, its tarball in an artifact, and its name in the publish order in `release.yml`'s npm job (binary packages before the package that depends on them). Keep its version and pins in `scripts/bump-version.cjs`.
2. Build its tarball for the current version and publish it once by hand (Step 1 above).
3. Add its trusted publisher (Step 2).
4. Cut a release within 2 days, so the new entry is validated.

## Verifying a release

```
npm view signalk-lume-ti dist-tags.latest
npm view @deepbluedynamics/lume dist-tags.latest
npm view @deepbluedynamics/lume dist.attestations.provenance.predicateType   # a URL here = signed by GitHub
```

Then try it:

```
npm install -g @deepbluedynamics/lume
lume --version
```

On npmjs.com, each package's trusted publisher should change from "Pending validation" to validated.

## When something goes wrong

| What you see | What it means | Fix |
|---|---|---|
| `E404 Not Found - PUT https://registry.npmjs.org/<pkg>` in the release's npm job | npm didn't accept GitHub's login: no trusted publisher entry, or one that doesn't match (owner case, workflow name, Allow npm publish unticked) | Fix the entry on npmjs.com (delete and re-add), then rerun only the npm job: `gh run rerun <run-id> -R DeepBlueDynamics/lume --failed` |
| `E404 … /-/v1/done?authId=…` on a hand publish | The browser approval timed out | Run the publish again; press Enter in the terminal when asked, then approve |
| `EOTP: This operation requires a one-time password` | `npm publish` ran somewhere it can't ask for approval (not a real terminal) | Run it in a normal terminal window |
| `git ls-remote ssh://git@github.com/npm/...` error | npm read `npm/file.tgz` as a GitHub repo name (fixed in #50) | Always give tarball paths with `./` in front |
| A version shows on GitHub but not npm | The npm job is the last step; check it in the Actions run | Fix the cause, then rerun with `--failed`. Already-published packages are skipped |
| `0.0.0-stage` version on a package | npm's placeholder while it processes a new upload | Wait a few minutes |

**Rerun caveat:** `gh run rerun --failed` uses the workflow file *as of that release's tag*. If the bug was in the workflow itself, a rerun repeats it; you need a new release after the fix is merged.

## Where things live in the repo

| Path | Purpose |
|---|---|
| `plugins/signalk-lume-ti/` | The plugin (its `package.json` pins the two binary packages) |
| `npm/lume/` | The CLI launcher package and its tests |
| `scripts/package-plugin.cjs` | Builds the three plugin tarballs |
| `scripts/package-cli.cjs` | Builds the six CLI tarballs |
| `scripts/bump-version.cjs` | Sets one version in Cargo.toml, Cargo.lock and both package.json files, including the pins |
| `.github/workflows/bump-version.yml` | The release button: bump, tag, then run release.yml |
| `.github/workflows/release.yml` | Build, GitHub release, then the **Publish to npm** job |
| `docs/NPM.md` | This guide |
| `docs/HALOS.md` | HaLOS install guide; §9 covers other Signal K servers (App Store, npm, offline tarballs) |
