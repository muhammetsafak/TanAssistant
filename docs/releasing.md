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
release with the files and `SHA256SUMS.txt` (a pre-release for `-rc` style tags).
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
