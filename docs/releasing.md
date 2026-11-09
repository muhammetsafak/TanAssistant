# Releasing

## Version

The version lives in `[workspace.package]` of the root `Cargo.toml`; `ui/package.json` carries
the same number. `src-tauri/tauri.conf.json` has no version of its own and follows the Cargo one.
To release:

1. Change the version in both files.
2. Move the `[Unreleased]` entries of `CHANGELOG.md` under a new version heading.
3. Commit as `chore(release): vX.Y.Z`, then push an annotated tag:

```bash
git tag -a v0.1.0 -m "v0.1.0"
git push origin v0.1.0
```

Use `vX.Y.Z-rc.1` for a pre-release.

## Release Workflow

`.github/workflows/release.yml` builds the Windows NSIS installer, the macOS `.app` and `.dmg`
(Apple Silicon) and the Linux `.deb` and AppImage on their own runners, and publishes a GitHub
release with the files, the signed update files, `latest.json` and `SHA256SUMS.txt` (a
pre-release for `-rc` style tags).
*Actions > Release > Run workflow* builds the same bundles without a tag and
without a release (artifacts only).

Release bundles use the GPU backends (Vulkan on Windows and Linux, Metal on macOS) and the
portable CPU baseline of `.cargo/config.toml`.

## macOS Signing

Signing and notarization use these repository secrets (*Settings > Secrets and variables >
Actions*). Their values never go into the repository or the logs.

| Secret | Content |
|---|---|
| `APPLE_CERTIFICATE` | The *Developer ID Application* certificate with its private key, exported as `.p12` and base64-encoded |
| `APPLE_CERTIFICATE_PASSWORD` | The password chosen when exporting the `.p12` |
| `APPLE_SIGNING_IDENTITY` | The certificate name, for example `Developer ID Application: <NAME> (<TEAM_ID>)` |
| `APPLE_ID` | The Apple ID e-mail used for notarization |
| `APPLE_PASSWORD` | An app-specific password of that Apple ID, not the account password |
| `APPLE_TEAM_ID` | The 10-character team id |

Without the first three the workflow still builds the macOS bundle unsigned and prints a warning;
without the last three it signs but does not notarize. Windows and Linux bundles are not
code-signed.

## Updater Signing

The in-app updater (`tauri-plugin-updater`) installs only update files signed with the project's
minisign key. The public half is `plugins.updater.pubkey` in `src-tauri/tauri.conf.json`; the
private half is a repository secret. `bundle.createUpdaterArtifacts` makes `tauri build` sign
every bundle with it.

### Generating the key

Done once, on a trusted machine (not in CI):

```bash
./ui/node_modules/.bin/tauri signer generate -w ~/.tauri/tan.key
```

The command asks for a password and writes `tan.key` (private) and `tan.key.pub` (public). The
content of `tan.key.pub` is the `pubkey` value in `tauri.conf.json`. Keep `tan.key` and its
password in a password manager; neither goes into the repository.

### Secrets

| Secret | Content |
|---|---|
| `TAURI_SIGNING_PRIVATE_KEY` | The content of `tan.key` (the file's text, not its path) |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | The password chosen when generating the key |

Without `TAURI_SIGNING_PRIVATE_KEY` the workflow still builds the installers, but with
`createUpdaterArtifacts` switched off, and prints a warning: no `.sig` files, no `.app.tar.gz`
and no `latest.json` are produced, so installed copies do not see that release.

### What a release carries for the updater

| Platform (updater key) | Update file | Signature |
|---|---|---|
| `windows-x86_64` | `*-setup.exe` (NSIS installer) | `*-setup.exe.sig` |
| `darwin-aarch64` | `Tan.app.tar.gz` | `Tan.app.tar.gz.sig` |
| `linux-x86_64` | `*.AppImage` | `*.AppImage.sig` |

The `.deb` has no update file; a `.deb` install is updated by hand.

The `Publish release` job writes `latest.json` from these: `version` (the tag without `v`),
`notes` (the link of the release), `pub_date` (UTC, RFC 3339) and, per platform, the content of
the `.sig` file as `signature` and the download URL of the update file in this release. If any of
the three signatures is missing, no `latest.json` is written and the log warns, because a
half-filled manifest would break the update check of the missing platform. `SHA256SUMS.txt`
covers `latest.json` too.

The updater reads `https://github.com/muhammetsafak/TanAssistant/releases/latest/download/latest.json`.
GitHub's `latest` skips pre-releases, so an `-rc` tag does not reach installed copies.

### Building the bundles locally

`createUpdaterArtifacts` is on in `tauri.conf.json`, so `tauri build` with a bundle target fails
with "A public key has been found, but no private key" on a machine without the signing key.
For a local bundle build, switch the update files off:

```bash
./ui/node_modules/.bin/tauri build --features metal --bundles app,dmg --config '{"bundle":{"createUpdaterArtifacts":false}}'
```

(or export `TAURI_SIGNING_PRIVATE_KEY` and its password to get signed update files).
`--no-bundle` builds, as in CI, are not affected.

### Trying it without a tag

*Actions > Release > Run workflow* builds the bundles and keeps them as artifacts. With the
secrets configured, each bundle artifact must list the `.sig` files (`tan-windows`:
`*.exe.sig`; `tan-macos`: `*.app.tar.gz` and `*.app.tar.gz.sig`; `tan-linux`:
`*.AppImage.sig`), and the log of *Build the bundles* says `Updater: the bundles will be signed`.
`latest.json` itself is written only by a tag run.

### If the key is lost

Installed copies verify updates against the public key built into them. A release signed with a
new key is rejected by every existing installation, so they cannot update themselves: users have
to install the new version by hand (download the installer from the release). Generate a new key,
replace `pubkey` in `tauri.conf.json` and the two secrets, and release; from that version on the
updater works again. A leaked private key is handled the same way, and the old key must be
considered compromised.
