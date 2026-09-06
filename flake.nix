{
  description = "mdr — A lightweight Markdown viewer with Mermaid diagram support";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        inherit (pkgs) lib;
        cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
      in {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = cargoToml.package.name;
          version = cargoToml.package.version;
          src = ./.;

          # Cargo.lock is tracked in the repository, so the dependency set is
          # derived from it instead of a hand-maintained cargoHash.
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = with pkgs; [
            pkg-config
          ] ++ lib.optionals stdenv.isLinux [
            wrapGAppsHook
          ];

          # On Darwin the WebKit/AppKit frameworks come from the default SDK of
          # the stdenv (darwin.apple_sdk.frameworks was removed from nixpkgs),
          # so no extra buildInputs are needed there.
          buildInputs = with pkgs; lib.optionals stdenv.isLinux [
            gtk3
            webkitgtk_4_1
            libxdo
            libGL
          ];

          meta = with lib; {
            description = cargoToml.package.description;
            homepage = cargoToml.package.homepage;
            license = licenses.mit;
            maintainers = [];
            mainProgram = "mdr";
          };
        };

        devShells.default = pkgs.mkShell {
          inputsFrom = [ self.packages.${system}.default ];
          packages = with pkgs; [
            rust-analyzer
            clippy
            rustfmt
          ];
        };
      });
}
