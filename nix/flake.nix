{
  description = "Development-only inputs for harbor-bevy";

  inputs = {
    nixpkgs-dev.url = "github:NixOS/nixpkgs/8edc0c72e3a38faf5434e40d1f19431125fe4b30";
    treefmt-nix = {
      url = "github:numtide/treefmt-nix/27b3b12a8e6375f28ebe122f07d230ca5459bbfa";
      inputs.nixpkgs.follows = "nixpkgs-dev";
    };
  };

  outputs = _: {};
}
