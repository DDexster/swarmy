# Homebrew formula for the tap koorchik/homebrew-tap (`brew install koorchik/tap/swarmy`).
# This file is the source of truth: the release workflow fills in @VERSION@, @REPO@ and the
# @SHA256_*@ checksums from the release's SHA256SUMS (`tools/dist.sh formula`) and commits the
# result to the tap as Formula/swarmy.rb. Do not edit the tap's copy by hand.
class Swarmy < Formula
  desc "Live swarm, spend and limits of Claude Code, Antigravity and Codex agents"
  homepage "@REPO@"
  license any_of: ["MIT", "Apache-2.0"]

  # `brew install --HEAD koorchik/tap/swarmy` builds the main branch from source.
  head do
    url "@REPO@.git", branch: "main"
    depends_on "rust" => :build
  end

  # Prebuilt static binaries: one universal binary for macOS (Apple silicon and Intel), musl
  # builds for Linux.
  if OS.mac?
    url "@REPO@/releases/download/v@VERSION@/swarmy-@VERSION@-universal-apple-darwin.tar.gz"
    sha256 "@SHA256_MACOS@"
  elsif Hardware::CPU.arm?
    url "@REPO@/releases/download/v@VERSION@/swarmy-@VERSION@-aarch64-unknown-linux-musl.tar.gz"
    sha256 "@SHA256_LINUX_AARCH64@"
  else
    url "@REPO@/releases/download/v@VERSION@/swarmy-@VERSION@-x86_64-unknown-linux-musl.tar.gz"
    sha256 "@SHA256_LINUX_X86_64@"
  end

  def install
    if build.head?
      system "cargo", "install", *std_cargo_args
      (man1/"swarmy.1").write Utils.safe_popen_read(bin/"swarmy", "man")
      generate_completions_from_executable(bin/"swarmy", "completions")
    else
      bin.install "swarmy"
      man1.install "man/swarmy.1"
      bash_completion.install "completions/swarmy.bash" => "swarmy"
      zsh_completion.install "completions/_swarmy"
      fish_completion.install "completions/swarmy.fish"
    end
  end

  test do
    assert_match(/^swarmy \d+\.\d+\.\d+/, shell_output("#{bin}/swarmy --version"))
    assert_match "swarmy", shell_output("#{bin}/swarmy completions bash")
    assert_match "swarmy", shell_output("#{bin}/swarmy man")
  end
end
