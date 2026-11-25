{
  harbor-rs,
  harbor-meta,
  rustfmtPackage,
}: {
  imports = [
    harbor-rs.treefmtModules.rust
    harbor-meta.treefmtModules.nix
    harbor-meta.treefmtModules.toml
  ];
  projectRootFile = "flake.nix";
  programs.rustfmt = {
    edition = "2024";
    package = rustfmtPackage;
  };
}
