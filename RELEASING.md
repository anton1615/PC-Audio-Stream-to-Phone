# Releasing AS2P

Everything about building a **signed, publishable** build. Read this before you push a tag.

> [!IMPORTANT]
> The release signing key is **not** in this repository. A public repository that contains the
> private key lets anyone sign an APK that Android treats as an update of this app. That is why
> `android/app/debug.keystore` was removed in v1.3.4 — the old key had been public since 2026-01-22
> and was used for published releases. It is considered burned.

## TL;DR

| | |
|---|---|
| Keystore location (local) | `C:\Users\Anton\.as2p\as2p-release.keystore` (outside any repo) |
| Alias | `as2p` |
| Type | PKCS12, RSA 4096, SHA384withRSA, 10000 days |
| Cert SHA-256 | `E1:34:83:51:DD:FB:7A:E1:38:6C:CD:82:2F:BB:FB:90:92:6D:A7:BC:EA:0A:5E:F7:58:B2:8E:9E:2E:29:D0:59` |
| CI secrets | `AS2P_KEYSTORE_B64`, `AS2P_KEYSTORE_PASSWORD`, `AS2P_KEY_ALIAS`, `AS2P_KEY_PASSWORD` |
| Password storage | Password manager (secure note) + offline USB copy. **Never** in the repo, never in a commit message. |

## The rules

1. **The private key never enters git.** No keystore, no `keystore.properties`, no base64 blob, no
   "temporary" commit. `*.keystore`, `*.jks` and `keystore.properties` are git-ignored.
2. **The key is backed up in at least two places, one of them offline.** GitHub Actions secrets are
   *not* a backup — secrets cannot be read back.
3. **The key is never reused across apps.** One key per app identity.
4. **CI verifies the certificate** of the APK it publishes. If the key ever changes, the expected
   SHA-256 in `.github/workflows/release.yml` must be updated in the same commit.

## Create a key (new app or rotation)

```bash
keytool -genkeypair -v \
  -keystore as2p-release.keystore -storetype PKCS12 \
  -alias as2p -keyalg RSA -keysize 4096 -validity 10000 \
  -storepass "<password>" -keypass "<password>" \
  -dname "CN=AS2P,OU=AS2P,O=Anton,L=Taipei,C=TW"
```

Use a generated 40-character random password, not a memorable one. PKCS12 requires the key
password to match the store password.

## Back it up (do this immediately after creating the key)

1. Copy `as2p-release.keystore` into your password manager as an encrypted attachment.
2. Copy it to an offline USB stick that is not connected to the machine during normal work.
3. Record the alias, the type and the certificate SHA-256 (see `keytool -list -v`) next to it.
4. **Verify a restore** — this is the step that actually proves the backup works:

   ```bash
   keytool -list -v -keystore <restored-copy> -storepass "<password>"
   apksigner sign --ks <restored-copy> --ks-pass pass:"<password>" --ks-key-alias as2p \
       --out test.apk app-release.apk
   ```

   A key that cannot be restored is not a backup.

## Losing the key

The private key inside a keystore is **not recoverable**. Not from the certificate, not from the
fingerprint, not from the APK. If every copy is gone:

- GitHub Actions secrets do **not** help — they cannot be read back.
- You must create a **new** key. The new APK will not match the signature of the installed app, so
  existing installs cannot be updated: users have to uninstall the old app and install the new one
  (app state — the latency/quality preferences in `AS2P_Prefs` — is lost).
- Ship the new key in a release and say so in the release notes under "Upgrade Notes".

## Release procedure

1. Make sure the working tree is clean and the version is bumped
   (`android/app/build.gradle` → `versionName`; `versionCode` is derived from the build time).
2. Tag and push: `git tag -a vX.Y.Z -m "AS2P vX.Y.Z — subject"` then `git push origin main && git push origin vX.Y.Z`.
3. `.github/workflows/release.yml` builds `server.exe` and the release APK, verifies the APK
   certificate, and creates the GitHub Release.
4. Rewrite the release notes — the CI-generated notes are just the tag message:

   ```bash
   gh release edit vX.Y.Z --title "AS2P vX.Y.Z — subject" --notes-file notes.md
   ```

### Local release build

```bash
# android/keystore.properties (git-ignored) — storeFile may be absolute or relative to android/
storeFile=C:/Users/Anton/.as2p/as2p-release.keystore
storePassword=<password>
keyAlias=as2p
keyPassword=<password>

cd android
gradle assembleRelease
```

The build **fails loudly** if a release variant is requested without signing material, so you can
never publish an unsigned APK by accident.

### Checking what signed an APK

```bash
apksigner verify --print-certs app-release.apk
```

Compare the SHA-256 with the value in this file and in the CI workflow.
