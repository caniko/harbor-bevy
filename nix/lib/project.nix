{
  harbor-rs,
  bevyDeps,
  stableToolchain,
  defaultCross,
  mkDevShells,
  mkAndroid,
  mkTestCluster,
  mkSteamTools,
  app,
}: {
  pkgs,
  src,
  pname,
  version,
  binary ? pname,
  toolchain ? stableToolchain pkgs [],
  cross ? defaultCross pkgs,
  x11 ? false,
  cargoArgs ? {},
  crossTargets ? [],
  crossTargetArgs ? {},
  fragpipe ? null,
  steampipe ? null,
  android ? null,
  cluster ? null,
  steam ? null,
  shellArgs ? {},
  checks ? {},
  extraApps ? {},
  extraPackages ? {},
}: let
  inherit (pkgs) lib;
  inherit (toolchain) craneLib;
  deps = bevyDeps {inherit pkgs x11;};
  commonArgs =
    {
      inherit src version;
      cargoExtraArgs = "--bin ${lib.escapeShellArg binary}";
      nativeBuildInputs = [pkgs.pkg-config];
    }
    // cargoArgs;
  nativeArgs =
    commonArgs
    // {
      inherit pname;
      buildInputs =
        lib.optionals pkgs.stdenv.hostPlatform.isLinux deps.buildInputs
        ++ (cargoArgs.buildInputs or []);
    }
    // (crossTargetArgs.native or {});
  native = craneLib.buildPackage (nativeArgs
    // {
      cargoArtifacts = craneLib.buildDepsOnly nativeArgs;
    });
  crossPackages = harbor-rs.lib.mkCrossPackages {
    inherit pkgs craneLib cross pname commonArgs;
    targets = lib.remove "native" crossTargets;
    targetArgs =
      lib.recursiveUpdate {
        aarch64-linux.buildInputs =
          (bevyDeps {
            pkgs = cross.linuxAarch64.pkgsCross;
            inherit x11;
          }).buildInputs
          ++ (cargoArgs.buildInputs or []);
      }
      crossTargetArgs;
  };
  androidOutputs =
    if android == null
    then null
    else
      mkAndroid ({
          inherit pkgs fragpipe;
          workspaceSrc = src;
        }
        // android);
  clusterOutputs =
    if cluster == null
    then null
    else
      assert lib.assertMsg (steampipe != null) "cluster configuration requires a steampipe argument";
        mkTestCluster ({
            inherit pkgs steampipe;
            projectRoot = src;
          }
          // cluster);
  steamTools =
    if steam == null
    then null
    else mkSteamTools pkgs steam;
in {
  inherit toolchain checks;
  devShells = mkDevShells ({
      inherit pkgs craneLib cross x11 fragpipe steampipe steam;
      android = androidOutputs;
    }
    // shellArgs);
  packages =
    {
      default = native;
      ${pname} = native;
    }
    // crossPackages
    // lib.optionalAttrs (androidOutputs != null) androidOutputs.packages
    // lib.optionalAttrs (steamTools != null) steamTools.packages
    // extraPackages;
  apps =
    {default = app "${native}/bin/${binary}";}
    // lib.optionalAttrs (androidOutputs != null) androidOutputs.apps
    // lib.optionalAttrs (clusterOutputs != null) clusterOutputs.apps
    // lib.optionalAttrs (steamTools != null) {steam-runtime-exec = app (lib.getExe steamTools.steamRuntimeExec);}
    // extraApps;
}
