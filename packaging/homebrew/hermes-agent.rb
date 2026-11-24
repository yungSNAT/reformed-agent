class HermesAgentUltra < Formula
  desc "Reformed Agent autonomous AI agent"
  homepage "https://github.com/yungSNAT/reformed-agent"
  version "0.22.0"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/yungSNAT/reformed-agent/releases/download/v0.22.0/hermes-macos-aarch64.tar.gz"
      sha256 "584bda4bb4524f813a2503a7a42cd3945a40f8ec4ed6a1d005bd82d822ba2399"
    else
      url "https://github.com/yungSNAT/reformed-agent/releases/download/v0.22.0/hermes-macos-x86_64.tar.gz"
      sha256 "47c263f51c5fe701622645c29e2780288ede1fdcce6e29a43965fca4e2982ed9"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/yungSNAT/reformed-agent/releases/download/v0.22.0/hermes-linux-aarch64.tar.gz"
      sha256 "a08450ed39f12cea79bf2f7a842d326508c1449852a83d4bcb6505edae731187"
    else
      url "https://github.com/yungSNAT/reformed-agent/releases/download/v0.22.0/hermes-linux-x86_64.tar.gz"
      sha256 "76b1fc251a1314fc30ca9a6b78708071d214a3709fa8a563378126d70b781afc"
    end
  end

  def install
    bin.install "hermes" => "hermes-agent-ultra"
    bin.install_symlink "hermes-agent-ultra" => "hermes-ultra"
  end

  test do
    system "#{bin}/hermes-agent-ultra", "--version"
  end
end
