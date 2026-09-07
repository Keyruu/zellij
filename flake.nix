{
  description = "Zellij fork with horizontal stacked-list (IDE-style) tabs";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "zellij";
          version = "0.46.0-fork";

          # Whole repo minus the huge cargo target dir and VCS metadata.
          src = pkgs.lib.cleanSourceWith {
            src = pkgs.lib.cleanSource ./.;
            filter = path: _type: baseNameOf path != "target";
          };

          cargoLock.lockFile = ./Cargo.lock;

          # Link against nixpkgs libcurl / openssl instead of building vendored copies
          # (vendored builds need perl and are pointless under nix).
          postPatch = ''
            substituteInPlace Cargo.toml \
              --replace-fail ', "vendored_curl"' ""
          '';
          env.OPENSSL_NO_VENDOR = 1;

          nativeBuildInputs = [
            pkgs.pkg-config
            (pkgs.lib.getDev pkgs.curl)
          ];
          buildInputs = [
            pkgs.curl
            pkgs.openssl
          ];

          # Test suite wants a writable HOME and is slow; the point here is a runnable binary.
          doCheck = false;

          meta.mainProgram = "zellij";
        };
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ self.packages.${pkgs.system}.default ];
          packages = [
            pkgs.cargo
            pkgs.rustc
            pkgs.rust-analyzer
            pkgs.clippy
            pkgs.rustfmt
          ];
          # curl/openssl -sys crates need pkg-config to find system libs.
          env.OPENSSL_NO_VENDOR = 1;
        };
      });

      apps = forAllSystems (pkgs: {
        default = {
          type = "app";
          program = "${self.packages.${pkgs.system}.default}/bin/zellij";
        };
      });
    };
}
