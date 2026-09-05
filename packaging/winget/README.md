# winget manifests (templates)

The files in this directory are **templates for the manifests submitted to winget-pkgs**.
They cannot be submitted as-is. Replace (i.e. render) `<VERSION>` / `<SHA256>` /
`<PRODUCT_CODE>` for each release before use.

> **Submission preconditions (owner policy)**
> Submitting to winget-pkgs is allowed only **after a release has been published in this repository, the released binaries have been verified to work, and the owner has given explicit approval**.
> There is no automatic submission. [.github/workflows/winget-publish.yml](../../.github/workflows/winget-publish.yml) is
> `workflow_dispatch` only and cannot run without entering the confirmation phrase.

## Files

winget-pkgs **does not allow singleton manifests for new packages**.
The three-file set of version / installer / defaultLocale is required.

| File | ManifestType | Contents |
|----------|--------------|------|
| `771-8bit.serial-monitor-essential.yaml` | `version` | Package ID and version, plus the default-locale declaration only |
| `771-8bit.serial-monitor-essential.installer.yaml` | `installer` | Installer type (nullsoft / user scope), URL, SHA-256, update matching keys |
| `771-8bit.serial-monitor-essential.locale.en-US.yaml` | `defaultLocale` | Display name, description, license, various URLs |

`PackageIdentifier` and `PackageVersion` **must match across all three files**; a mismatch fails validation.
The schema version is 1.6.0. If winget-pkgs has raised the required version by the time you
submit, bump `ManifestVersion` in all three files together.

The location in winget-pkgs is `manifests/7/771-8bit/serial-monitor-essential/<VERSION>/`
(the path is derived from `PackageIdentifier`).

### Values to replace

| Placeholder | How to obtain |
|----------------|----------|
| `<VERSION>` | `version` in `src-tauri/tauri.conf.json` (the single source of truth for the version; docs/25 §1.2). The tag is `v<VERSION>` |
| `<SHA256>` | Download the `.exe` from the Release and run `Get-FileHash -Algorithm SHA256 <file>`. **Compute it from the published Release asset, not from a freshly built local artifact** (this doubles as a check that the upload was not corrupted) |
| `<PRODUCT_CODE>` | See "Determining the ProductCode" below |

### Determining the ProductCode

Tauri's NSIS installer writes its uninstall information to **HKCU** (per-user).
winget looks there to decide "already installed? / update needed?", so the actual observed
value must be used. **Install the first-release binary for real**, then look up the actual
key name with:

```powershell
Get-ChildItem 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall' |
  ForEach-Object { $p = Get-ItemProperty $_.PSPath
    if ($p.DisplayName -eq 'serial-monitor-essential') { $_.PSChildName } }
```

Put the printed key name into `AppsAndFeaturesEntries[].ProductCode`.
**Do not fill it in by guessing.** A wrong ProductCode breaks silently, in the form of
"an update exists but is never detected".

### Why NSIS only

The Release carries both NSIS (`.exe`, per-user) and MSI (`.msi`, per-machine) installers,
but only NSIS is registered with winget, for two reasons:

- winget's default install runs without administrator rights. The per-user NSIS installer
  requires no elevation.
- Mixing per-user and per-machine in the same package splits update detection by
  ProductCode, causing double installs.

The MSI stays on the Release for deployment tooling in corporate environments (docs/25 §5.1).

## Publishing procedure

Follow the order. **Do not run step 4 while skipping steps 1–3.**

1. **Publish the release** — complete the checklist in docs/25 §4 and publish the draft
   Release. While it remains a draft, the asset URLs cannot be fetched from outside and
   winget's validation pipeline fails.
2. **Verify the published binaries** — download the installer from the Release and confirm
   install → launch → COM connection → plotter rendering → exit (exit 0) (docs/25 §4 step 8).
3. **Obtain the owner's explicit approval.**
4. **Run `Winget Publish (manual, approval-gated)` via workflow_dispatch** —
   enter a version in `0.1.0` form for `version` and `I-HAVE-OWNER-APPROVAL` for `confirm`.

### The first submission can be a manual PR

`wingetcreate update` is **for updating an existing package** and cannot be used for the
first submission. For the first one, pick one of the following. A manual PR is the most
reliable and the easiest for responding to review feedback.

- **Manual PR (recommended)**: render the three files in this directory, place them at
  `manifests/7/771-8bit/serial-monitor-essential/<VERSION>/` in a fork of
  [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs), and open a PR.
  Validate locally before submitting: `winget validate --manifest <dir>` and
  `winget install --manifest <dir>` (the latter actually installs, so uninstall after checking).
- **`wingetcreate new`**: generates the manifests interactively. Diff the output against
  these templates before submitting.
- **`komac new`**: equivalent. Either komac or wingetcreate is fine; the workflow uses
  wingetcreate.

From the second release on, `wingetcreate update` (i.e. the workflow path) can be used.

### About review

- The first submission takes **several days** of winget-pkgs review. After the automated
  validation passes (manifest schema, URL reachability, SHA-256 match, install test,
  malware scan), a human review follows.
- Because there is **no code signing**, SmartScreen / SmartScreen-derived warnings may be
  raised during validation (docs/25 §6 R-1 / TBD-RS4).
- The `PackageIdentifier` spelling (`771-8bit` starts with a digit) may draw a review
  request to change the publisher spelling. If asked, change it consistently across all
  three files.

## Required secrets

| Secret | Purpose | Permissions |
|--------------|------|------|
| `WINGET_TOKEN` | GitHub PAT that lets `wingetcreate` fork microsoft/winget-pkgs, push a branch, and open a PR | `public_repo` for a classic PAT. For a fine-grained PAT: Contents: Read and write + Pull requests: Read and write on "All public repositories" |

- Register it as `WINGET_TOKEN` under the repository's Settings → Secrets and variables → Actions.
- **`GITHUB_TOKEN` cannot be used**: it has no permission to fork or create PRs against
  another repository (winget-pkgs).
- If the secret is not registered, the workflow fails with an explicit message before doing
  any work.
- Token management (expiry, reissue on revocation) is tracked in docs/25 §6 TBD-RS7.
