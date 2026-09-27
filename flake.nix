{
  description = "ldmux — tmux-native remote terminal (daemon + CLI + mobile)";

  # Main toolchain pin: the same nixpkgs-unstable revision used by the dooboxes
  # flake, so the source tree and binary cache are shared on this machine.
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/6cdc7fc76e8bf7fde9fa43a849fcaaa70e230dee";
  # Second pin, for the Node runtime only. The revision above tops out at
  # nodejs_24; Node 26 is the Current line (26.8.2) and is what the daemon and
  # CLI run on. Pinning a second input keeps that upgrade from dragging the
  # whole toolchain forward at the same time.
  inputs.nixpkgs-node.url = "github:NixOS/nixpkgs/c7def046b9a883d46974757852106483d741586f";

  outputs = { self, nixpkgs, nixpkgs-node }:
    let
      # v1 targets Linux; keep darwin in the matrix for future mobile/desktop work.
      systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f {
        inherit system;
        pkgs = import nixpkgs { inherit system; };
        nodePkgs = import nixpkgs-node { inherit system; };
      });

      # What the daemon *runs on*: exactly these, in the tests and in the
      # deployment. They were different — the suite ran this tmux 3.7b and Node 26
      # while the unit ran Debian's tmux 3.5a and an nvm Node 25 — and a profile
      # option that crashes 3.5a passed every test and took the real daemon down.
      runtime = pkgs: nodePkgs: [ nodePkgs.nodejs_26 pkgs.tmux ];

      # The dev toolchain: the runtime, plus conveniences.
      toolchain = pkgs: nodePkgs: runtime pkgs nodePkgs ++ (with pkgs; [
        just
        jq
        git
        curl
        sqlite
        procps
        # A QR *decoder*, for tests only: the encoder behind `ldmux code` is judged
        # by reading every code it makes back (packages/daemon/test/qr.test.ts).
        zbar
      ]);
    in
    {
      devShells = forAllSystems ({ pkgs, nodePkgs, ... }: {
        default = pkgs.mkShell {
          packages = toolchain pkgs nodePkgs;
          shellHook = ''
            echo "ldmux devshell"
            echo "  node  $(node --version)"
            echo "  npm   $(npm --version)"
            echo "  tmux  $(tmux -V)"
          '';
        };

        # The Android/KMP toolchain (JDK + Gradle + Zig). Kept separate from the
        # default shell because it is only needed to build packages/mobile and
        # the native libghostty-vt core. `nix develop .#mobile`
        mobile = pkgs.mkShell {
          packages = toolchain pkgs nodePkgs ++ [ pkgs.jdk25 pkgs.gradle pkgs.zig ];
          JAVA_HOME = "${pkgs.jdk25}";
          shellHook = ''
            export ANDROID_HOME="$HOME/Android/Sdk"
            export ANDROID_SDK_ROOT="$ANDROID_HOME"
            # libghostty-vt's build locates the NDK via ANDROID_NDK_HOME. Never
            # hard-code a version, and never sort the *directory names*: SDK
            # Manager installs side-by-side revisions as both `android-ndk-rNNx`
            # and `<revision>`, and `sort -V` orders a digit-leading name before a
            # letter-leading one — so with r27c and 30.0.16248370 both installed it
            # silently picks the *older* toolkit. Resolve by the Pkg.Revision each
            # directory declares, which is the version SDK Manager itself reports.
            export ANDROID_NDK_HOME="$(
              for p in "$ANDROID_HOME"/ndk/*/source.properties; do
                [ -f "$p" ] || continue
                echo "$(sed -n 's/^Pkg.Revision *= *//p' "$p") $(dirname "$p")"
              done | sort -V | tail -1 | cut -d' ' -f2-
            )"
            echo "ldmux mobile devshell"
            echo "  java   $(${pkgs.jdk25}/bin/javac -version 2>&1)"
            echo "  gradle $(gradle --version 2>/dev/null | awk '/^Gradle/{print $2}')"
            echo "  zig    $(zig version)"
            echo "  sdk    $ANDROID_HOME"
            echo "  ndk    $ANDROID_NDK_HOME"
          '';
        };
      });

      # `just install` builds this to ~/.local/state/ldmux/runtime and runs the
      # daemon, CLI and desk windows from it. That link is also a GC root, so a
      # `nix-collect-garbage` cannot take the running daemon's tmux away.
      packages = forAllSystems ({ pkgs, nodePkgs, ... }: {
        runtime = pkgs.buildEnv {
          name = "ldmux-runtime";
          paths = runtime pkgs nodePkgs;
        };
        default = self.packages.${pkgs.stdenv.hostPlatform.system}.runtime;
      });

      # `nix fmt`
      formatter = forAllSystems ({ pkgs, ... }: pkgs.nixpkgs-fmt);

      # NOTE: reproducible builds of ldmuxd/ldmux (via npm) and the KMP/Android
      # app are added in later phases. Phase 1 only needs the devShell
      # (node + tmux + tests), which is fully reproducible here.
    };
}