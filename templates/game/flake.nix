{
  description = "Stock Bevy 2D game starter";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/8edc0c72e3a38faf5434e40d1f19431125fe4b30";
    flake-parts.url = "github:hercules-ci/flake-parts/31729ca8cbdb4fa927b34e5f4353e6a83f39e993";
    harbor-bevy = {
      url = "github:caniko/harbor-bevy";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.flake-parts.follows = "flake-parts";
    };
  };

  outputs = inputs @ {harbor-bevy, ...}:
    harbor-bevy.lib.mkBevyFlake {
      inherit inputs;
      project = _: {
        src = ./.;
        pname = "harbor-game";
        version = "0.1.0";
        shellArgs.visualTest = false;
      };
    };
}
