{
  description = "ldmux — remote terminal sessions (daemon + CLI + keeper + mobile)";

  # Two inputs, both pinned in flake.lock. Nothing else is fetched at flake level.
  #
  # nixpkgs: the main toolchain (rustc, cargo, zig 0.16, glibc, test tools). The same
  # nixpkgs-unstable revision as the dooboxes flake, so the store and binary cache are
  # shared on this machine.
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/6cdc7fc76e8bf7fde9fa43a849fcaaa70e230dee";
  # nixpkgs-node: Node 26 only, and Node is a DEV tool (the mobile `.ts` tests and
  # scripts/slow-link.ts, via Node's built-in type stripping). The main pin tops out at
  # nodejs_24; the revision below provides 26.8.2. Nothing in the Rust build, the checks
  # or the deployed daemon needs it.
  inputs.nixpkgs-node.url = "github:NixOS/nixpkgs/c7def046b9a883d46974757852106483d741586f";

  # Not flake inputs, but pinned and fetched by hash from inside the build:
  #   - Ghostty (the terminal engine): ONE full commit + a fixed-output hash, both in
  #     packages/mobile/native/ghostty.pin (the Android build script reads the same file).
  #   - Ghostty's own zig dependencies: its build.zig.zon.nix at that commit lists each
  #     tarball with a hash (fetchzip/fetchgit, fixed-output), so the zig build runs in
  #     the sandbox with `--system <deps>` and no network.
  #   - Cargo crates: from Cargo.lock (importCargoLock, checksummed by the lockfile).

  outputs = { self, nixpkgs, nixpkgs-node }:
    let
      # darwin is not here: libghostty-vt's nix build was not evaluated or built on darwin
      # and the daemon is Linux-only (PTY/proc/systemd/i3). Add it back only with a darwin
      # builder to prove it.
      systems = [ "x86_64-linux" "aarch64-linux" ];
      lib = nixpkgs.lib;
      forAllSystems = f: lib.genAttrs systems (system: f {
        inherit system;
        pkgs = import nixpkgs { inherit system; };
        nodePkgs = import nixpkgs-node { inherit system; };
      });

      # ---- the engine pin: one file, read by this flake and by the Android scripts ----
      pin =
        let
          parse = l:
            let m = builtins.match "([a-z-]+)[ \t]+([^ \t#]+)[ \t]*" l;
            in lib.optional (m != null) { name = builtins.elemAt m 0; value = builtins.elemAt m 1; };
          attrs = lib.listToAttrs (lib.concatMap parse
            (lib.splitString "\n" (builtins.readFile ./packages/mobile/native/ghostty.pin)));
        in
        assert lib.assertMsg (builtins.stringLength (attrs.commit or "") == 40)
          "packages/mobile/native/ghostty.pin: `commit` must be a full 40-char Ghostty commit";
        assert lib.assertMsg (attrs ? source-hash) "ghostty.pin: missing `source-hash`";
        attrs;

      ghosttySrc = pkgs: pkgs.fetchFromGitHub {
        owner = "ghostty-org";
        repo = "ghostty";
        rev = pin.commit;
        hash = pin.source-hash;
      };

      # libghostty-vt, built by Ghostty's OWN nix expression at the pinned commit (so the
      # build flags are upstream's), with Zig from the pinned nixpkgs and optimize =
      # ReleaseSafe (the mode the Android build uses). `callPackage` on a path inside the
      # fetched source is import-from-derivation: evaluation fetches the (fixed-output)
      # source. Outputs: `.dev` holds lib/libghostty-vt.a, `.out` the shared lib + headers.
      engineFor = pkgs: pkgs.callPackage "${ghosttySrc pkgs}/nix/libghostty-vt.nix" {
        zig_0_16 = pkgs.zig_0_16;
        revision = builtins.substring 0 7 pin.commit;
        optimize = "ReleaseSafe";
      };

      # ---- the Rust workspace ----
      version = "0.1.0";
      cargoLock = { lockFile = ./Cargo.lock; };

      # Sources for the shipped binaries: the workspace plus deploy/ (launcher, unit template).
      srcFor = lib.fileset.toSource {
        root = ./.;
        fileset = lib.fileset.unions [ ./Cargo.toml ./Cargo.lock ./crates ./deploy ];
      };
      # The test suite also reads the desk-window launcher and the Kotlin modifier table
      # (crates/daemon/src/stream.rs checks it against the wire contract).
      testSrc = lib.fileset.toSource {
        root = ./.;
        fileset = lib.fileset.unions [
          ./Cargo.toml
          ./Cargo.lock
          ./crates
          ./deploy
          ./packages/mobile/composeApp/src/commonMain/kotlin/io/ldmux/mobile/Modifiers.kt
        ];
      };

      ldmuxFor = pkgs:
        let engine = engineFor pkgs; in
        pkgs.rustPlatform.buildRustPackage {
          pname = "ldmux";
          inherit version cargoLock;
          src = srcFor;
          # vt-sys/build.rs links the engine archive from here.
          LDMUX_VT_LIB_DIR = "${engine.dev}/lib";
          # The tests are a separate check (`checks.<system>.tests`): they need the debug
          # build layout, the engine, zbar and (optionally) Xvfb/i3.
          doCheck = false;
          # The desk-window launcher and the unit template ride with the binaries, so a
          # store path is a complete install source (see `apps.install`).
          postInstall = ''
            install -Dm755 deploy/bin/ldmux-terminal $out/bin/ldmux-terminal
            install -Dm644 deploy/ldmux.service $out/share/ldmux/ldmux.service
            install -Dm644 deploy/ldmux-keeper.service $out/share/ldmux/ldmux-keeper.service
            install -Dm644 deploy/ldmux.env.example $out/share/ldmux/ldmux.env.example
          '';
          meta = {
            description = "ldmux: daemon (ldmuxd), CLI (ldmux), keeper (ldmux-keeper), attach client";
            mainProgram = "ldmux";
            platforms = systems;
          };
        };

      # Tools the tests use when present. zbarimg is REQUIRED (the QR tests fail without
      # it); the X tools are optional (desktop_e2e skips, naming the missing tool).
      testTools = pkgs: [ pkgs.zbar pkgs.xorg-server pkgs.i3 pkgs.xterm pkgs.procps pkgs.git ];

      # `cargo test --workspace` in the sandbox. Debug profile on purpose: several
      # integration tests exec `target/debug/ldmux-keeper` / `target/debug/ldmux`, so the
      # workspace is built in debug first, exactly as the gate does on a dev machine.
      cargoCheck = pkgs: name: script: extra:
        pkgs.stdenv.mkDerivation ({
          inherit name;
          src = testSrc;
          cargoDeps = pkgs.rustPlatform.importCargoLock cargoLock;
          nativeBuildInputs = [ pkgs.rustPlatform.cargoSetupHook pkgs.cargo pkgs.rustc ];
          LDMUX_VT_LIB_DIR = "${(engineFor pkgs).dev}/lib";
          dontConfigure = false;
          buildPhase = ''
            runHook preBuild
            export HOME=$TMPDIR
            ${script}
            runHook postBuild
          '';
          installPhase = "touch $out";
          dontFixup = true;
        } // extra);

      toolchainTools = pkgs: [ pkgs.rustfmt pkgs.clippy pkgs.rustc pkgs.cargo ];
    in
    {
      packages = forAllSystems ({ pkgs, ... }: {
        default = ldmuxFor pkgs;
        ldmux = ldmuxFor pkgs;
        # The engine, so `nix build .#libghostty-vt` prints the exact thing the Rust build
        # links, and a hash mismatch after a pin bump shows up here first.
        libghostty-vt = engineFor pkgs;
      });

      devShells = forAllSystems ({ pkgs, nodePkgs, ... }:
        let
          engine = engineFor pkgs;
          # `python3` runs `scripts/test-parallel.py`, the gate.
          common = toolchainTools pkgs ++ testTools pkgs ++ [ nodePkgs.nodejs_26 pkgs.jq pkgs.curl pkgs.sqlite pkgs.cargo-deny pkgs.cargo-audit pkgs.python3 ];
        in
        {
          # `nix develop` + plain `cargo build/test/clippy`. The engine is a nix build
          # (zig, hermetic), made once and cached; LDMUX_VT_LIB_DIR points vt-sys at it.
          default = pkgs.mkShell {
            packages = common;
            LDMUX_VT_LIB_DIR = "${engine.dev}/lib";
            LDMUX_VT_INCLUDE_DIR = "${engine.dev}/include";
            shellHook = ''
              echo "ldmux devshell"
              echo "  rustc $(rustc --version)"
              echo "  node  $(node --version)   (dev only: mobile tests, scripts/)"
              echo "  engine $LDMUX_VT_LIB_DIR"
            '';
          };

          # The Android/KMP toolchain (JDK + Gradle + Zig). Separate because it is only
          # needed for packages/mobile and the native libghostty-vt cross-builds.
          # `nix develop .#mobile`
          mobile = pkgs.mkShell {
            packages = common ++ [ pkgs.jdk25 pkgs.gradle pkgs.zig_0_16 pkgs.git ];
            JAVA_HOME = "${pkgs.jdk25}";
            LDMUX_VT_LIB_DIR = "${engine.dev}/lib";
            LDMUX_VT_INCLUDE_DIR = "${engine.dev}/include";
            # Ghostty's zig dependency cache (fixed-output). build-libghostty-vt.sh passes it
            # as `zig build --system`, so the Android cross-build needs no network for deps.
            LDMUX_GHOSTTY_ZIG_DEPS = "${engine.deps}";
            shellHook = ''
              export ANDROID_HOME="''${ANDROID_HOME:-$HOME/Android/Sdk}"
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

      checks = forAllSystems ({ pkgs, nodePkgs, ... }: {
        # The binaries build (and the engine links).
        build = self.packages.${pkgs.stdenv.hostPlatform.system}.default;

        # The gate: `cargo test --workspace`, debug, in the sandbox.
        # Tests needing a tailnet or a real DISPLAY are not run here by design:
        # they skip (desktop_e2e names the missing tool) or use loopback only.
        tests = cargoCheck pkgs "ldmux-tests" ''
          cargo build --workspace --offline
          # Tests that need an FHS root (/usr/bin, /bin/bash, /usr, populated /usr/lib):
          # the build sandbox has only /bin/sh and /usr/bin/env. They are skipped here BY
          # NAME and run in the dev-shell gate and in CI (`cargo test` on a real host).
          # `--skip` is a substring match, so the guard below lists every test first and
          # fails the check unless each name below matches exactly ONE test: a rename or
          # a new test sharing a prefix cannot silently widen or void the skip set.
          skips="
            the_desktop_inventory_links_a_running_pi_to_its_session
            a_window_opens_without_any_window_manager
            the_emulator_has_its_own_process_group
            a_background_job_does_not_survive_the_shell
            a_killed_keeper_ends_its_shells_even_when_a_later_one_ignores_hup
            a_valid_cwd_is_entered
            compression_cuts_the_wire_cost_of_every_real_workload
          "
          cargo test --workspace --offline -- --list 2>/dev/null | grep ': test$' > all-tests.txt
          args=""
          for s in $skips; do
            n=$(grep -c "$s: test" all-tests.txt || true)
            if [ "$n" != 1 ]; then echo "skip '$s' matches $n tests (want exactly 1)" >&2; exit 1; fi
            args="$args --skip $s"
          done
          echo "SKIPPED (FHS-only): $skips"
          cargo test --workspace --offline --no-fail-fast -- $args
        '' { nativeBuildInputs = [ pkgs.rustPlatform.cargoSetupHook pkgs.cargo pkgs.rustc ] ++ testTools pkgs; };

        # `cargo clippy --workspace --all-targets -- -D warnings`
        clippy = cargoCheck pkgs "ldmux-clippy" ''
          cargo clippy --workspace --all-targets --offline -- -D warnings
        '' { nativeBuildInputs = [ pkgs.rustPlatform.cargoSetupHook pkgs.cargo pkgs.rustc pkgs.clippy ]; };

        # The mobile tree's own TypeScript tests (no device needed). They read the Rust
        # protocol sources too (the pane ceiling), so the repo layout is kept.
        mobile-test = pkgs.runCommand "ldmux-mobile-test"
          {
            nativeBuildInputs = [ nodePkgs.nodejs_26 pkgs.git ];
            src = lib.fileset.toSource {
              root = ./.;
              fileset = lib.fileset.unions [ ./crates ./packages/mobile ./Cargo.toml ];
            };
          } ''
          cp -r $src ./repo && chmod -R u+w ./repo
          cd repo
          # architecture.test.ts asks git which Kotlin sources are tracked.
          git init -q && git add -A
          node --test packages/mobile/test/*.test.ts
          touch $out
        '';
      });

      # Runnable tasks that have real logic. Everything else is a plain command
      # (README "Development"): cargo build / test / clippy, `nix develop`, `nix build`.
      apps = forAllSystems ({ pkgs, nodePkgs, ... }:
        let
          system = pkgs.stdenv.hostPlatform.system;
          ldmux = self.packages.${system}.default;
          app = drv: { type = "app"; program = "${drv}/bin/${drv.name}"; };
        in
        {
          default = { type = "app"; program = "${ldmux}/bin/ldmux"; };
          # Install the nix-built binaries for this user (systemd --user unit, launcher,
          # ~/.local/bin links, GC root). `nix run .#install`; FORCE=1 to overwrite the unit.
          install = app (pkgs.writeShellScriptBin "ldmux-install" ''
            export LDMUX_DEPLOY=${./deploy}
            exec ${pkgs.runtimeShell} ${./scripts/install.sh} ${ldmux}/bin "$@"
          '');
          restart-keeper = app (pkgs.writeShellScriptBin "ldmux-restart-keeper" ''
            exec ${pkgs.runtimeShell} ${./scripts/restart-keeper.sh} "$@"
          '');
          mobile-test = app (pkgs.writeShellScriptBin "ldmux-mobile-test" ''
            [ -d packages/mobile/test ] || { echo "run from the repo root" >&2; exit 1; }
            exec ${nodePkgs.nodejs_26}/bin/node --test packages/mobile/test/*.test.ts
          '');
        });

      # `nix fmt`
      formatter = forAllSystems ({ pkgs, ... }: pkgs.nixpkgs-fmt);
    };
}
