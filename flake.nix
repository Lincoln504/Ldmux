{
  description = "ldmux — remote terminal sessions (daemon + CLI + keeper + mobile)";

  # Main toolchain pin: the same nixpkgs-unstable revision used by the dooboxes
  # flake, so the source tree and binary cache are shared on this machine.
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/6cdc7fc76e8bf7fde9fa43a849fcaaa70e230dee";
  # Second pin, for the Node runtime only. The revision above tops out at
  # nodejs_24; Node 26 is the Current line (26.8.2), and the dev scripts under
  # `scripts/` run on it. Pinning a second input keeps that upgrade from dragging
  # the whole toolchain forward at the same time.
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

      # What the deployed binaries link against *dynamically*: the C library and
      # libgcc from this flake's toolchain. The daemon is a cargo build from this
      # shell, so it links these exact store paths; `just install` links this
      # derivation as a GC root so a `nix-collect-garbage` cannot delete them out
      # from under the running daemon. tmux and Node used to be here — the daemon
      # runs on neither any more (the keeper owns the PTYs, the daemon is Rust).
      runtime = pkgs: _nodePkgs: [ pkgs.glibc pkgs.libgcc ];

      # The Rust toolchain (the cargo/rustc from nixpkgs, used by the Rust
      # port). The rustup toolchain in the devShell is the system one;
      # the flake's `rustPlatform` provides a pinned rustc + cargo for
      # reproducible builds.
      rustToolchain = pkgs: with pkgs; [ rustfmt clippy rustc cargo ];

      # Development-only extras: not part of what the daemon runs on.
      devExtras = pkgs: nodePkgs: [ nodePkgs.nodejs_26 pkgs.just pkgs.jq pkgs.git pkgs.curl pkgs.sqlite pkgs.procps
        # A QR *decoder*, for tests only: the Rust encoder behind `ldmux code`
        # is judged by reading every code it makes back (crates/daemon/src/qr.rs).
        pkgs.zbar ];

      # The dev toolchain: the runtime, plus conveniences.
      toolchain = pkgs: nodePkgs: runtime pkgs nodePkgs ++ rustToolchain pkgs ++ devExtras pkgs nodePkgs;
    in
    {
      devShells = forAllSystems ({ pkgs, nodePkgs, ... }: {
        default = pkgs.mkShell {
          packages = toolchain pkgs nodePkgs;
          shellHook = ''
            echo "ldmux devshell"
            echo "  node  $(node --version)"
            echo "  npm   $(npm --version)"
            echo "  rustc $(rustc --version)"
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
            # The debug signing key. Gradle keeps it in $ANDROID_USER_HOME, falling
            # back to ~/.android, and on this machine the key every installed build
            # was signed with lives in ~/.config/.android. A shell without the
            # variable made a *new* key in ~/.android, and the phone then refused the
            # update ("signatures do not match") until the app was uninstalled, which
            # wipes its pairing. Measured 2026-09-27. Use the existing key when there
            # is one, and never override a value someone set on purpose.
            if [ -z "''${ANDROID_USER_HOME:-}" ] && [ -f "$HOME/.config/.android/debug.keystore" ]; then
              export ANDROID_USER_HOME="$HOME/.config/.android"
            fi
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

      # `just install` builds this to ~/.local/state/ldmux/runtime and keeps it
      # as a GC root, so `nix-collect-garbage` cannot take the C library the
      # running daemon links against.
      packages = forAllSystems ({ pkgs, nodePkgs, ... }: {
        runtime = pkgs.buildEnv {
          name = "ldmux-runtime";
          paths = runtime pkgs nodePkgs;
        };
        default = self.packages.${pkgs.stdenv.hostPlatform.system}.runtime;
      });

      # `nix fmt`
      formatter = forAllSystems ({ pkgs, ... }: pkgs.nixpkgs-fmt);
    };
}