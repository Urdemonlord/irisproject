# Draft Homebrew formula for the IRIS macOS one-package install.
#
# DRAFT: the sha256 values below are placeholders. Fill them from the
# release page (or `shasum -a 256 <tarball>`) once the first tagged
# release carries the macOS tarballs; until then `brew install` from
# this formula cannot work. Do not guess hashes.
#
# The formula only delivers the files into the Cellar. The system
# setup (iris user, /etc/iris, /etc/stalwart, /etc/caddy, launchd
# services) still runs through packaging/macos/iris-install-macos.sh
# with sudo; see the caveats and packaging/homebrew/README.md.
class IrisMailStack < Formula
  desc "Self-hosted mail stack: IRIS webmail + Stalwart + Caddy, one install"
  homepage "https://github.com/Urdemonlord/irisproject"
  version "0.4.0"
  license all_of: ["MIT", "Apache-2.0"]

  depends_on macos: ">= :sonoma"

  on_arm do
    url "https://github.com/Urdemonlord/irisproject/releases/download/v#{version}/iris-mail-stack-#{version}-macos-arm64.tar.gz"
    sha256 "TODO-arm64-sha256-fill-from-release"
  end

  on_intel do
    url "https://github.com/Urdemonlord/irisproject/releases/download/v#{version}/iris-mail-stack-#{version}-macos-amd64.tar.gz"
    sha256 "TODO-amd64-sha256-fill-from-release"
  end

  def install
    # The tarball is rooted at usr/local, etc, var, Library. Homebrew
    # installs into the Cellar, so relocate the payload under prefix;
    # the setup script (run with sudo, see caveats) wires the system
    # paths afterwards.
    bin.install Dir["usr/local/bin/*"]
    sbin.install Dir["usr/local/sbin/*"]
    (lib/"iris").install Dir["usr/local/lib/iris/*"]
    (share/"iris").install Dir["usr/local/share/iris/*"]
    (pkgshare/"launchd").install Dir["Library/LaunchDaemons/*.plist"]
  end

  def caveats
    <<~EOS
      The files are installed, but the mail stack is not set up yet.
      Finish the install as root (it creates the iris user, /etc/iris,
      /etc/stalwart, /etc/caddy and the launchd services, then
      provisions Stalwart and IRIS):

        sudo IRIS_DOMAIN=example.com #{lib}/iris/iris-install-macos.sh

      Notes:
        - On Intel Macs, `brew link` puts symlinks at the /usr/local
          paths the setup script expects, so it just works. On Apple
          Silicon (prefix /opt/homebrew) the script needs a PREFIX
          adapter first; see packaging/homebrew/README.md.
        - Ports 25, 80 and 443 must be free, and DNS for
          mail.<domain> (plus inbox.<domain> and
          mail-content.<domain>) must point at this Mac.
        - Check the install with: #{sbin}/iris-health
    EOS
  end

  test do
    assert_predicate bin/"iris-api", :exist?
    assert_match "1.0.3", shell_output("#{bin}/stalwart --version")
    assert_match "2.11.9", shell_output("#{bin}/caddy version")
  end
end
