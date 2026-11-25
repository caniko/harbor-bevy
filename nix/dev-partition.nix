{
  inputs,
  self,
  ...
}: {
  perSystem = {pkgs, ...}: let
    inherit (inputs) harbor-rs harbor-meta;
    tools = harbor-rs.lib.mkToolchain {
      inherit pkgs;
      toolchainProfile = "stable";
      targets = [];
    };
    treefmt = inputs.treefmt-nix.lib.evalModule pkgs (import ./treefmt.nix {
      inherit harbor-rs harbor-meta;
      rustfmtPackage = tools.rustToolchain;
    });
    deps = self.lib.bevyDeps {inherit pkgs;};
    x11 = self.lib.bevyDeps {
      inherit pkgs;
      x11 = true;
    };
    visual = self.lib.mkVisualTestEnv {
      inherit pkgs;
      extraPackages = [pkgs.hello];
      extraEnv.HARBOR_BEVY_TEST = "1";
    };
  in {
    formatter = treefmt.config.build.wrapper;
    devShells.default =
      (self.lib.mkDevShells {
        inherit pkgs;
        inherit (tools) craneLib;
        extraPackages = [pkgs.python3 treefmt.config.build.wrapper];
      }).default;

    checks = {
      denylist =
        pkgs.runCommand "harbor-bevy-denylist" {
          nativeBuildInputs = [pkgs.findutils pkgs.gnugrep];
        } ''
          bash ${self}/scripts/check-denylist.sh --snapshot > "$out"
        '';
      lib-eval = assert deps.pkgConfigDeps != [];
      assert builtins.length x11.runtimeLibs > builtins.length deps.runtimeLibs;
      assert builtins.isString deps.ldLibraryPath;
      assert builtins.elem pkgs.hello visual.packages;
      assert visual.env.HARBOR_BEVY_TEST == "1";
      assert visual.env.WGPU_BACKEND == "vulkan";
      assert pkgs.lib.hasSuffix ".json" visual.env.VK_DRIVER_FILES;
        pkgs.writeText "harbor-bevy-lib-eval" "Bevy dependencies and Mesa RADV evaluate\n";
    };
  };
}
