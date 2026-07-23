{
  description = "chrome-use — drive your real, logged-in Chrome from any AI agent";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
  };

  outputs = inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];

      perSystem = { pkgs, self', ... }: {
        packages.default = pkgs.callPackage ./nix/package.nix { };
        packages.chrome-use = self'.packages.default;

        apps.default = {
          type = "app";
          program = "${self'.packages.default}/bin/chrome-use";
        };

        devShells.default = pkgs.mkShell {
          inputsFrom = [ self'.packages.default ];
          packages = with pkgs; [
            cargo
            rustc
            clippy
            rustfmt
            nodejs_24
            pnpm
            chromium
            vhs
            jq
            ripgrep
          ];
        };

        checks.build = self'.packages.default;

        formatter = pkgs.nixpkgs-fmt;
      };
    };
}
