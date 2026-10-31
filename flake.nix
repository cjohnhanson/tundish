{
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # tests/merge_gate_guard.rs drives scripts/merge-gate.sh, which
    # refuses to run without gaff on PATH. The check phase needs the
    # binary, and the sandbox has no other way to reach it.
    gaff = {
      url = "github:cjohnhanson/gaff";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.crane.follows = "crane";
      inputs.rust-overlay.follows = "rust-overlay";
    };
  };

  outputs =
    {
      nixpkgs,
      crane,
      rust-overlay,
      gaff,
      ...
    }:
    let
      forEachSystem =
        f:
        nixpkgs.lib.genAttrs
          [
            "aarch64-darwin"
            "x86_64-linux"
            "aarch64-linux"
            "x86_64-darwin"
          ]
          (
            system:
            f {
              pkgs = import nixpkgs {
                inherit system;
                overlays = [ rust-overlay.overlays.default ];
              };
              inherit system;
            }
          );
    in
    {
      packages = forEachSystem (
        { pkgs, system }:
        let
          toolchain = pkgs.rust-bin.stable.latest.default;
          craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;

          # crane's default source filter strips non-Rust files; the
          # bundled docs and missouri fixtures must survive the filter.
          src = pkgs.lib.cleanSourceWith {
            src = ./.;
            filter =
              path: type:
              (craneLib.filterCargoSources path type)
              || (builtins.match ".*tests/missouri/.*" path != null)
              || (builtins.match ".*\\.yml$" path != null)
              || (builtins.match ".*\\.missouri.*" path != null)
              || (builtins.match ".*/docs$" path != null)
              || (builtins.match ".*/docs/.*" path != null)
              || (builtins.match ".*/skills$" path != null)
              || (builtins.match ".*/skills/.*" path != null)
              # tests/merge_gate_guard.rs runs scripts/merge-gate.sh, so the
              # script has to reach the sandbox. Without it five of seven
              # cases fail on a missing file and the package cannot build,
              # while the gate stays green because a runner has the script.
              || (builtins.match ".*/scripts$" path != null)
              || (builtins.match ".*/scripts/.*" path != null);
          };

          commonArgs = {
            # The version is read from the manifest rather than
            # repeated. pyproject.toml declares it dynamic and
            # npm/generate.mjs greps it, so this was the one place a
            # release had to edit by hand, and no gate reads this file.
            pname = "tundish";
            version = (craneLib.crateNameFromCargoToml { cargoToml = ./Cargo.toml; }).version;
            inherit src;
            strictDeps = true;
            buildInputs = pkgs.lib.optionals pkgs.stdenv.isDarwin [
              pkgs.libiconv
            ];
          };

          cargoArtifacts = craneLib.buildDepsOnly commonArgs;

          tool = craneLib.buildPackage (
            commonArgs
            // {
              inherit cargoArtifacts;
              # Man pages and shell completions come from the built
              # binary itself, so they always match the real CLI.
              postInstall = ''
                mkdir -p $out/share/man/man1
                $out/bin/tundish gen-man $out/share/man/man1
                mkdir -p $out/share/zsh/site-functions
                mkdir -p $out/share/bash-completion/completions
                mkdir -p $out/share/fish/vendor_completions.d
                $out/bin/tundish gen-completions zsh > $out/share/zsh/site-functions/_tundish
                $out/bin/tundish gen-completions bash > $out/share/bash-completion/completions/tundish
                $out/bin/tundish gen-completions fish > $out/share/fish/vendor_completions.d/tundish.fish
              '';
              # Unit tests only in the nix check; the missouri suite runs in
              # development (it needs the missouri binary, which lives in its
              # own derivation).
              # tests/merge_gate_guard.rs drives scripts/merge-gate.sh,
              # which runs git and gaff and reads the event payload with
              # jq. stdenv builds PATH from the declared inputs, so all
              # three have to be named here. A runner has them already,
              # which is why the gate stays green while the package
              # cannot build.
              nativeCheckInputs = [
                pkgs.gitMinimal
                pkgs.jq
                # gaff's own check phase runs its merge-gate fixtures,
                # which fail inside a sandbox. The binary is all this
                # check needs, so gaff's tests stay in gaff's own CI.
                (gaff.packages.${system}.default.overrideAttrs { doCheck = false; })
              ];
              checkPhase = ''
                tmpHome="$(mktemp -d)"
                export HOME="$tmpHome"
                cargo test --profile release --locked
              '';
            }
          );
        in
        {
          default = tool;
          tundish = tool;
        }
      );

      devShells = forEachSystem (
        { pkgs, ... }:
        {
          default = pkgs.mkShell {
            buildInputs = [
              pkgs.rust-bin.stable.latest.default
              pkgs.jq
            ]
            ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
              pkgs.libiconv
            ];
          };
        }
      );
    };
}
