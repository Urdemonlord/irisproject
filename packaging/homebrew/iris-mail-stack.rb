# Homebrew formula for the IRIS macOS one-package install.
#
# The sha256 values below were filled from the v0.5.0 release tarballs
# (download the asset, `shasum -a 256 <tarball>`). Update the version in
# both URLs and both hashes together when a new release ships; never guess them.
#
# The formula only delivers the files into the Cellar. The system
# setup (iris user, /etc/iris, /etc/stalwart, /etc/caddy, launchd
# services) still runs through packaging/macos/iris-install-macos.sh
# with sudo; see the caveats and packaging/homebrew/README.md.
class IrisMailStack < Formula
  desc "Self-hosted mail stack: IRIS webmail + Stalwart + Caddy, one install"
  homepage "https://github.com/Urdemonlord/irisproject"
  license all_of: ["MIT", "Apache-2.0"]

  depends_on macos: :sonoma

  if Hardware::CPU.arm?
    url "https://github.com/Urdemonlord/irisproject/releases/download/v0.5.0/iris-mail-stack-0.5.0-macos-arm64.tar.gz"
    sha256 "5314eff9d8a3e0ca8a4210f80867a2283ee1b1f29a1e839475b37ec039957a6b"
  else
    url "https://github.com/Urdemonlord/irisproject/releases/download/v0.5.0/iris-mail-stack-0.5.0-macos-amd64.tar.gz"
    sha256 "c1e4f205c74e7cb9274cac1d8928f4cf967bcdf1434d6036c9cae7b9299fca8d"
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
    assert_path_exists bin/"iris-api"
    assert_match "1.0.3", shell_output("#{bin}/stalwart --version")
    assert_match "2.11.9", shell_output("#{bin}/caddy version")
  end
end
