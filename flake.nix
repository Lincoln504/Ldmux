{
  description = "ldmux — tmux-native remote terminal (pi extension + daemon + CLI + mobile)";

  # Pinned to the same nixpkgs-unstable revision used by the dooboxes flake so
  # the source tree and binary cache are shared on this machine.
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/6cdc7fc76e8bf7fde9fa43a849fcaaa70e230dee";

  outputs = { self, nixpkgs }:
    let
      # v1 targets Linux; keep darwin in the matrix for future mobile/desktop work.
      systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f {
        inherit system;
        pkgs = import nixpkgs { inherit system; };
      });

      # The runtime + dev toolchain. Bun is the daemon/CLI/extension runtime;
      # tmux is the backend under test; the rest are dev conveniences.
      toolchain = pkgs: with pkgs; [
        bun
        nodejs_24
        tmux
        just
        jq
        git
        curl
        sqlite
        procps
      ];
    in
    {
      devShells = forAllSystems ({ pkgs, ... }: {
        default = pkgs.mkShell {
          packages = toolchain pkgs;
          shellHook = ''
            echo "ldmux devshell"
            echo "  bun   $(bun --version)"
            echo "  node  $(node --version)"
            echo "  tmux  $(tmux -V)"
          '';
        };

        # The Android/KMP toolchain (JDK + Gradle + Zig). Kept separate from the
        # default shell because it is only needed to build packages/mobile and
        # the native libghostty-vt core. `nix develop .#mobile`
        mobile = pkgs.mkShell {
          packages = toolchain pkgs ++ [ pkgs.jdk25 pkgs.gradle pkgs.zig ];
          JAVA_HOME = "${pkgs.jdk25}";
          shellHook = ''
            export ANDROID_HOME="$HOME/Android/Sdk"
            export ANDROID_SDK_ROOT="$ANDROID_HOME"
            # libghostty-vt's build locates the NDK via ANDROID_NDK_HOME;
            # pick the newest installed one (sort -V), never hard-code a version.
            export ANDROID_NDK_HOME="$(ls -d "$ANDROID_HOME"/ndk/* 2>/dev/null | sort -V | tail -1)"
            echo "ldmux mobile devshell"
            echo "  java   $(${pkgs.jdk25}/bin/javac -version 2>&1)"
            echo "  gradle $(gradle --version 2>/dev/null | awk '/^Gradle/{print $2}')"
            echo "  zig    $(zig version)"
            echo "  sdk    $ANDROID_HOME"
            echo "  ndk    $ANDROID_NDK_HOME"
          '';
        };
      });

      # `nix fmt`
      formatter = forAllSystems ({ pkgs, ... }: pkgs.nixpkgs-fmt);

      # NOTE: reproducible builds of ldmuxd/ldmux (via bun2nix) and the
      # KMP/Android app are added in later phases. Phase 1 only needs the
      # devShell (bun + tmux + tests), which is fully reproducible here.
    };
}