{
  description = "Bevy development tooling and isolated replication workspaces";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/8edc0c72e3a38faf5434e40d1f19431125fe4b30";
    flake-parts.url = "github:hercules-ci/flake-parts/31729ca8cbdb4fa927b34e5f4353e6a83f39e993";
    harbor-meta = {
      url = "git+https://github.com/caniko/harbor-meta.git?ref=trunk&rev=de35b00324ebdb01200f69cae83e472041c43e54";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    harbor-rs = {
      url = "git+https://github.com/caniko/harbor-rs.git?ref=trunk&rev=fbb4f0c146cb70d1f4c7baf1837b2dd6f84c2c2d";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.flake-parts.follows = "flake-parts";
      inputs.harbor-meta.follows = "harbor-meta";
      inputs.harbor-android.follows = "harbor-android";
    };
    harbor-android = {
      url = "github:caniko/harbor-android/75aaa3fccaaedead1df8a236f3c9862200afce03";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.harbor-meta.follows = "harbor-meta";
    };
  };

  outputs = inputs @ {
    flake-parts,
    harbor-rs,
    harbor-meta,
    harbor-android,
    ...
  }:
    flake-parts.lib.mkFlake {inherit inputs;} {
      imports = [flake-parts.flakeModules.partitions];
      systems = ["x86_64-linux" "aarch64-linux"];

      flake.lib = import ./nix/lib {inherit harbor-rs harbor-meta harbor-android;};
      flake.nixosModules = import ./nix/modules;
      partitions.dev = {
        extraInputsFlake = ./nix;
        module = ./nix/dev-partition.nix;
      };
      partitionedAttrs = {
        checks = "dev";
        devShells = "dev";
        formatter = "dev";
      };
    };
}
