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
      });

      # `nix fmt`
      formatter = forAllSystems ({ pkgs, ... }: pkgs.nixpkgs-fmt);

      # NOTE: reproducible builds of ldmuxd/ldmux (via bun2nix) and the
      # KMP/Android app are added in later phases. Phase 1 only needs the
      # devShell (bun + tmux + tests), which is fully reproducible here.
    };
}