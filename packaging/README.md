# Releases and packaging

What a release ships, how it is built, and the one-time setup the optional parts need.

## What a release ships

A tag `v<version>` (the version in `Cargo.toml`) runs `.github/workflows/release.yml`:

| Target | Built on | Archive | Packages |
|---|---|---|---|
| `x86_64-unknown-linux-musl` | ubuntu, cargo-zigbuild | `.tar.gz` | `_amd64.deb`, `.x86_64.rpm` |
| `aarch64-unknown-linux-musl` | ubuntu, cargo-zigbuild | `.tar.gz` | `_arm64.deb`, `.aarch64.rpm` |
| `armv7-unknown-linux-musleabihf` (Raspberry Pi 2–5, 32-bit OS) | ubuntu, cargo-zigbuild | `.tar.gz` | `.armv7hl.rpm` |
| `arm-unknown-linux-musleabihf` (ARMv6: Pi Zero, Zero W, 1) | ubuntu, cargo-zigbuild | `.tar.gz` | `_armhf.deb` |
| `x86_64-pc-windows-msvc` (static CRT) | windows | `.zip` | |
| `universal-apple-darwin` (aarch64 + x86_64, `lipo`) | macos | `.tar.gz` | Homebrew formula |

Every archive is `swarmy-<version>-<target>/` with the binary, `README.md`, `CHANGELOG.md`,
both licences, `man/swarmy.1` and `completions/` (bash, zsh, fish, PowerShell, elvish), made by
`swarmy man` and `swarmy completions <shell>`. `SHA256SUMS` covers every file. The release notes
are the `## v<version>` section of `CHANGELOG.md`, or the tag's message when there is none.

There is no other channel: no distribution repositories, PPAs or Windows package managers. The
`.deb` and `.rpm` files are for installing by hand:

```sh
sudo apt install ./swarmy_<version>-1_amd64.deb    # sudo apt remove swarmy
sudo dnf install ./swarmy-<version>-1.x86_64.rpm   # sudo dnf remove swarmy
```

They hold the same static binary as the archive, the man page (`/usr/share/man/man1`), the
bash, zsh and fish completions in their standard places, and the docs; they depend on nothing.

### Why static musl binaries

A binary linked against glibc runs only on systems whose glibc is at least as new as the one it
was built with, so a build on a current Ubuntu fails on an older Debian, RHEL or Raspberry Pi OS,
and a build on an old system is needed to cover them all. A binary linked statically against musl
carries its C library: it asks for no dynamic loader and no shared library (the release checks
this with `readelf`: no `INTERP`, no `NEEDED`), so the same file runs on every distribution and
every release of it, old and new, glibc or musl (Alpine). SQLite is compiled in (the
`bundled-sqlite` feature) and there is no OpenSSL (swarmy makes no network calls), so nothing
else is needed.

The `.deb` and `.rpm` packages only wrap that binary, so they install on any release too.
Distributions that build from source and prefer their system SQLite can build with
`cargo build --release --no-default-features` (without `bundled-sqlite`).

One `armhf` `.deb` is shipped, built for ARMv6: Raspberry Pi OS's 32-bit `armhf` is ARMv6, so it
runs on every Raspberry Pi, and Debian's and Ubuntu's `armhf` (ARMv7) run it too. Two `armhf`
packages of one name could not be told apart. The ARMv7 build is in its archive and the
`armv7hl` `.rpm`.

## Making a release

1. Set `version` in `Cargo.toml`, add a `## v<version>` section to `CHANGELOG.md`, run
   `tools/check.sh`, commit.
2. Optionally try the Linux and Windows builds locally: `tools/release-local.sh` (below).
3. `git tag -a v<version> -m "swarmy <version>"` and `git push origin v<version>`.

The workflow fails early when the tag and `Cargo.toml` disagree. A version with a hyphen
(`0.2.0-rc.1`) becomes a pre-release and does not update the Homebrew tap.

## Building locally

`tools/release-local.sh` builds the Linux and Windows targets on a Linux x86_64 machine the way
the workflow does (it runs the same `tools/dist.sh` steps) into `dist/`:

```sh
tools/release-local.sh            # linux + windows
tools/release-local.sh linux      # the four musl targets, .deb, .rpm
tools/release-local.sh windows
NO_PACKAGES=1 tools/release-local.sh linux
```

It needs:

- zig 0.16 on `PATH` (or `ZIG_DIR=<dir with zig>`), `cargo install --locked cargo-zigbuild`,
  and `rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
  armv7-unknown-linux-musleabihf arm-unknown-linux-musleabihf`;
- for packages: `cargo install --locked cargo-deb cargo-generate-rpm`;
- for Windows: `cargo install --locked cargo-xwin`, LLVM (clang, lld, `llvm-lib`; found in
  `/usr/lib/llvm-*/bin` when not on `PATH`) and `rustup target add x86_64-pc-windows-msvc`.
  The script checks that `swarmy.exe` imports no Visual C++ runtime DLL.

Builds go to `$CARGO_TARGET_DIR` (or `target/`). The Windows binary runs under Wine
(`wine swarmy.exe --version`); the ARM binaries under `qemu-arm` / `qemu-aarch64` user-mode
emulation. macOS is built only by the workflow (it needs the SDK and `lipo`).

## Homebrew tap (optional)

`packaging/homebrew/swarmy.rb` is the formula's source of truth. After each release the
`homebrew` job fills in the version and checksums and commits it to the tap
`koorchik/homebrew-tap` as `Formula/swarmy.rb`; without the key (or token) the job says so and
does nothing. Users then run `brew install koorchik/tap/swarmy` (macOS universal binary, Linux x86_64
and aarch64), or `brew install --HEAD koorchik/tap/swarmy` to build `main` from source with
Homebrew's Rust.

One-time setup (done for `koorchik/swarmy`):

1. Create the public repository `koorchik/homebrew-tap` on GitHub (the `homebrew-` prefix is
   what makes `koorchik/tap` work) with a `README.md`; the job creates `Formula/` itself.
2. Give the workflow write access to that repository only, with a deploy key:
   ```sh
   ssh-keygen -t ed25519 -N "" -C "swarmy release → homebrew-tap" -f tapkey
   gh repo deploy-key add tapkey.pub --repo koorchik/homebrew-tap --allow-write --title "swarmy release workflow"
   gh secret set HOMEBREW_TAP_DEPLOY_KEY --repo koorchik/swarmy < tapkey
   rm tapkey tapkey.pub
   ```
   (A fine-grained personal access token with *Contents: Read and write* on the tap, saved as
   `HOMEBREW_TAP_TOKEN`, works too; the deploy key is preferred: it never expires and reaches
   one repository.)

To publish a formula by hand (or for the first release, before the secret exists): download
`SHA256SUMS` of the release into `dist/`, run `tools/dist.sh formula`, and commit
`dist/packaging/swarmy.rb` to the tap as `Formula/swarmy.rb`. Check it with
`brew install --build-from-source koorchik/tap/swarmy && brew test swarmy && brew audit --strict swarmy`.

## macOS signing and notarization (optional)

The macOS binary is unsigned for now. A file downloaded with a browser carries the quarantine
attribute and Gatekeeper refuses to run an unsigned one; users either remove the attribute once:

```sh
xattr -d com.apple.quarantine ./swarmy
```

or install with Homebrew, which does not quarantine formulae (and `curl` downloads are not
quarantined either).

When these repository secrets exist, the `macos` job signs the universal binary with the
Developer ID certificate and the hardened runtime, and sends it to Apple's notary service (a
bare binary cannot be stapled; Gatekeeper looks the ticket up online). Without them both steps
are skipped.

| Secret | What |
|---|---|
| `APPLE_CERTIFICATE_P12` | the *Developer ID Application* certificate with its private key, exported as `.p12` and base64-encoded: `base64 -i cert.p12 \| pbcopy` |
| `APPLE_CERTIFICATE_PASSWORD` | the password chosen when exporting the `.p12` |
| `APPLE_DEVELOPER_ID` | the signing identity, as `security find-identity -v -p codesigning` prints it: `Developer ID Application: <Name> (<TEAMID>)` |
| `APPLE_TEAM_ID` | the 10-character team ID (developer.apple.com → Membership) |
| `APPLE_NOTARY_APPLE_ID` | the Apple ID email of the developer account (notarization runs only when this is set too) |
| `APPLE_NOTARY_PASSWORD` | an app-specific password for that Apple ID (appleid.apple.com → Sign-In and Security → App-Specific Passwords) |

Getting the certificate: join the Apple Developer Program; in Xcode → Settings → Accounts →
Manage Certificates add a *Developer ID Application* certificate (or create one at
developer.apple.com → Certificates with a CSR from Keychain Access); in Keychain Access export
it with its private key as `.p12`. Add the secrets under Settings → Secrets and variables →
Actions. Check a signed download with `codesign --verify --strict --verbose=2 swarmy` and
`spctl --assess --type execute --verbose swarmy`.
