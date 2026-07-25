# macOS code signing + notarization

Runbook for shipping a Gatekeeper-clean `.dmg` from `.github/workflows/release.yml`. Everything
here is macOS-only; the Windows leg is unaffected.

## Why all of it is needed

An ad-hoc signed app (`"signingIdentity": "-"`, the local-dev default in `tauri.conf.json`) is
blocked on any machine that downloads it. To be openable by a normal double-click the artifact
needs **all** of:

1. a **Developer ID Application** signature (not "Apple Development", not "Apple Distribution"),
2. the **hardened runtime** enabled on the app,
3. **notarization** by Apple, and
4. a **stapled** ticket so first launch works offline.

## Secrets

| Secret                       | Required          | What it is                                                          |
| ---------------------------- | ----------------- | ------------------------------------------------------------------- |
| `APPLE_CERTIFICATE`          | signing           | base64 of the exported Developer ID Application `.p12`               |
| `APPLE_CERTIFICATE_PASSWORD` | signing           | the export password you typed when saving that `.p12`                |
| `APPLE_SIGNING_IDENTITY`     | signing           | `Developer ID Application: Your Name (TEAMID)` — exact string        |
| `APPLE_API_KEY`              | notarize (API)    | App Store Connect key **ID** (e.g. `2X9R4HXF34`)                     |
| `APPLE_API_ISSUER`           | notarize (API)    | issuer UUID from the same page                                       |
| `APPLE_API_KEY_BASE64`       | notarize (API)    | base64 of the downloaded `AuthKey_<KEYID>.p8`                        |
| `APPLE_ID`                   | notarize (AppleID)| Apple account email                                                  |
| `APPLE_PASSWORD`             | notarize (AppleID)| **app-specific** password, not the account password                  |
| `APPLE_TEAM_ID`              | notarize (AppleID)| 10-char team id                                                      |

Rules the workflow enforces:

- The three signing secrets go in **together or not at all** — a partial set fails the job instead
  of silently producing an ad-hoc build.
- Provide **one** notarization set. The API key wins if both are present.
- With no signing secrets the build still succeeds, ad-hoc, and logs a warning.
- `APPLE_SIGNING_IDENTITY` is not optional when signing: the Tauri CLI compares it against the
  imported certificate, and without it the ad-hoc `"-"` from `tauri.conf.json` is used as the
  identity and the comparison fails.

## Getting the certificate

Needs a **paid** Apple Developer Program membership, and only the **Account Holder** can create a
Developer ID certificate.

1. Xcode → Settings → Accounts → your team → *Manage Certificates…* → **+** → **Developer ID
   Application**. (Or Certificates, Identifiers & Profiles on developer.apple.com with a CSR from
   Keychain Access.)
2. Keychain Access → *My Certificates* → right-click the new `Developer ID Application: …` →
   **Export…** → `.p12`, set an export password. The export must include the private key — that is
   why it has to come from *My Certificates*, and why the arrow next to it must expand to a key.
3. Encode and copy:

   ```bash
   openssl base64 -A -in DeveloperID.p12 | pbcopy    # → APPLE_CERTIFICATE
   security find-identity -v -p codesigning          # → APPLE_SIGNING_IDENTITY (quoted string)
   ```

## Getting notarization credentials

**App Store Connect API key (preferred — no 2FA, revocable, not tied to a person):**

1. App Store Connect → Users and Access → *Integrations* → *App Store Connect API* → **+**.
   Access **Developer** is enough for notarization.
2. Download `AuthKey_<KEYID>.p8` (one download only), then:

   ```bash
   openssl base64 -A -in AuthKey_<KEYID>.p8 | pbcopy   # → APPLE_API_KEY_BASE64
   ```

   Key ID → `APPLE_API_KEY`, Issuer ID → `APPLE_API_ISSUER`.

**Apple ID alternative:** appleid.apple.com → Sign-In and Security → App-Specific Passwords →
generate one → `APPLE_PASSWORD`. Team id from developer.apple.com → Membership.

## What the pipeline does

1. `Configure macOS signing` exports only the secrets that are fully present into `$GITHUB_ENV`.
   This is deliberate: the Tauri CLI probes these with `var_os()`, so an empty-but-defined
   `APPLE_CERTIFICATE` reads as "signing requested" and dies in the keychain import.
2. The Tauri CLI (2.11.x) then does all of this itself during `tauri build`:
   creates a throwaway keychain → imports the `.p12` → `set-key-partition-list` → signs
   `Contents/Frameworks/*.dylib` (the bundled libheif closure) inside-out → signs the `.app` with
   `--options runtime` → `notarytool submit --wait` on a ditto-zipped `.app` → `stapler staple` →
   deletes the keychain. `APPLE_SIGNING_IDENTITY` from the environment overrides the ad-hoc `"-"`
   in `tauri.conf.json`, so no config patching happens in CI.
3. `Verify macOS signature` runs `codesign --verify --deep --strict` over the `.app` and each
   bundled dylib. This is the only place a bad `install_name_tool` rewrite from
   `scripts/macos-bundle-dylibs.sh` would surface before a user hits it.
4. `Notarize + staple DMG` covers Tauri's gap: the bundler signs the `.dmg` but never notarizes it,
   and a downloaded disk image is Gatekeeper-assessed in its own right. The step submits, staples,
   validates with `spctl --assess --type open`, and replaces the asset tauri-action already
   uploaded (`gh release upload --clobber`).

The minisign `TAURI_SIGNING_*` secrets are a separate mechanism (updater artifact signatures) and
have nothing to do with Apple signing.

## Verifying a release locally

```bash
# on a machine WITHOUT Homebrew libheif, ideally
xcrun stapler validate Darkroom_0.4.0_aarch64.dmg
spctl --assess --type open --context context:primary-signature -vv Darkroom_0.4.0_aarch64.dmg
codesign --display --verbose=4 /Applications/Darkroom.app       # Authority + TeamIdentifier + runtime flag
otool -L /Applications/Darkroom.app/Contents/MacOS/darkroom | grep heif   # → @executable_path/../Frameworks/
xattr -d com.apple.quarantine <dmg>   # only to re-test the "fresh download" path after clearing it
```

## Troubleshooting

| Symptom                                                                        | Cause / fix                                                                                                                         |
| ------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------- |
| `certificate from APPLE_CERTIFICATE "..." does not match provided identity "-"` | `APPLE_SIGNING_IDENTITY` missing, so the ad-hoc `"-"` was used. Set it.                                                             |
| Keychain import fails on a build with no certs                                  | An `APPLE_CERTIFICATE` secret exists but is empty. Delete it rather than blanking it.                                              |
| Notary log: `The signature does not include a secure timestamp`                 | `codesign`'s default timestamping did not kick in (no network to Apple's TSA). Re-run; if persistent, the signing call needs an explicit `--timestamp`. |
| Notary log: `The executable does not have the hardened runtime enabled`         | `hardenedRuntime` was turned off, or something signed the app after Tauri did.                                                     |
| Notary log: `The binary is not signed with a valid Developer ID certificate`    | An "Apple Development" cert was exported instead of "Developer ID Application".                                                     |
| App launches on the build machine but not elsewhere, `Library not loaded: …libheif` | The dylib closure was not staged/rewritten. Run `bash scripts/macos-bundle-dylibs.sh stage` and check `bundle.macOS.frameworks`. |
| Gatekeeper still blocks the DMG                                                 | The `Notarize + staple DMG` step was skipped — notarization secrets absent (`notary=none` in the job log).                          |

## First signed release checklist

- [ ] All signing secrets + one notarization set added to the repo.
- [ ] Tag a `beta-*` first — it publishes as a pre-release, so a failed attempt is not "Latest".
- [ ] Job log shows `Developer ID signing enabled for: …` and a notarization line, not the
      ad-hoc warning.
- [ ] Download the DMG through a browser (so it carries the quarantine bit) and open it on a Mac
      that has never built the app.
