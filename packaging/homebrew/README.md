# Homebrew formula (draft)

`iris-mail-stack.rb` is a draft Homebrew formula for the macOS
one-package install. It is a draft because the two things a formula
needs from a release do not exist yet: the macOS tarballs have never
been uploaded to any release (PR #31 automates that from the next
tagged release on), so there are no release URLs or SHA-256 hashes to
point at.

## What the formula does

- Downloads the per-arch tarball for the release version
  (`on_arm` / `on_intel` blocks, URLs follow the
  `iris-mail-stack-<ver>-macos-<arch>.tar.gz` naming from
  `packaging/macos/build-macos.sh`).
- Installs the payload into the Cellar: `iris-api`, `stalwart` and
  `caddy` to `bin`, `iris-health` and `iris-cert-sync` to `sbin`,
  the setup/provisioning scripts to `lib/iris`, the web dist and
  `Caddyfile.template` to `share/iris`, and the launchd plists to
  `pkgshare/launchd` for reference.
- It does **not** run the system setup. The formula cannot do that:
  creating the `iris` user, writing `/etc/iris`, `/etc/stalwart`,
  `/etc/caddy` and installing `/Library/LaunchDaemons` plists all
  need root. The user runs `iris-install-macos.sh` with sudo after
  installing (see the formula's caveats).

## Publishing the tap

When the first release with macOS tarballs exists:

1. Create the tap repo `Urdemonlord/homebrew-iris` (a normal public
   repo, no code needed).
2. Copy this file to `Formula/iris-mail-stack.rb` in that repo.
3. Fill in both `sha256` values from the release assets
   (`shasum -a 256 <tarball>`).
4. Users then run:
   `brew tap Urdemonlord/iris && brew install iris-mail-stack`
   followed by the sudo setup step from the caveats.

## What is still missing before this works

- **Release assets**: the first tagged release after PR #31 (the
  upload job) must exist so the URLs and hashes are real. Do not
  point the formula at CI artifacts; they expire and need auth.
- **PREFIX adapter for Apple Silicon**: `iris-install-macos.sh`
  hardcodes `/usr/local` paths. On Intel Macs Homebrew's prefix is
  `/usr/local` and `brew link` puts symlinks where the script
  expects them, so it works. On Apple Silicon (prefix
  `/opt/homebrew`) the script must learn a `PREFIX` variable first.
- **Testing**: `brew install --build-from-source`, `brew test` and
  the sudo setup step have never run; they need a real Mac and a
  real release. The `test` block's version assertions were checked
  against the pinned binaries (`stalwart --version` prints
  `Stalwart Server v1.0.3`; `caddy version` prints `v2.11.9`).
