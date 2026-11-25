{
  harbor-rs,
  harbor-meta,
  harbor-android,
}: let
  bevyDeps = {
    pkgs,
    x11 ? false,
  }: let
    x11Libs = pkgs.lib.optionals x11 (with pkgs; [libx11 libxcursor libxi libxrandr libxcb]);
    runtimeLibs = with pkgs; [vulkan-loader wayland libxkbcommon] ++ x11Libs;
    pkgConfigDeps = with pkgs; [alsa-lib udev zlib] ++ runtimeLibs;
  in {
    inherit pkgConfigDeps runtimeLibs;
    buildInputs = pkgConfigDeps ++ (with pkgs; [vulkan-headers vulkan-validation-layers]);
    ldLibraryPath = pkgs.lib.makeLibraryPath runtimeLibs;
  };

  mesaRadv = {
    pkgs,
    profile,
  }: let
    icd =
      {
        x86_64-linux = "radeon_icd.x86_64.json";
        aarch64-linux = "radeon_icd.aarch64.json";
      }.${
        pkgs.stdenv.hostPlatform.system
      } or (throw "Mesa RADV is unsupported on this system");
  in
    assert pkgs.lib.assertMsg (profile == "mesa-radv") "Unknown visual test profile: ${profile}"; {
      packages = [pkgs.mesa pkgs.vulkan-loader];
      env = {
        RS_HARBOR_GPU_PIN_PROFILE = profile;
        RS_HARBOR_GPU_EXPECTED_BACKEND = "Vulkan";
        RS_HARBOR_GPU_EXPECTED_VENDOR = "0x1002";
        RS_HARBOR_GPU_EXPECTED_DRIVER_CONTAINS = "radv";
        WGPU_BACKEND = "vulkan";
        VK_DRIVER_FILES = "${pkgs.mesa}/share/vulkan/icd.d/${icd}";
      };
    };

  mkVisualTestEnv = {
    pkgs,
    profile ? "mesa-radv",
    extraPackages ? [],
    extraEnv ? {},
  }: let
    pin = (harbor-rs.lib.mkGpuRenderPin or mesaRadv) {inherit pkgs profile;};
  in {
    packages = pin.packages ++ extraPackages;
    env = {HARBOR_BEVY_REQUIRE_HARDWARE_VULKAN = "1";} // pin.env // extraEnv;
  };

  stableToolchain = pkgs: targets:
    harbor-rs.lib.mkToolchain {
      inherit pkgs targets;
      toolchainProfile = "stable";
    };

  defaultCross = pkgs:
    harbor-rs.lib.mkCross {
      inherit pkgs;
      inherit (pkgs.stdenv.hostPlatform) system;
    };

  toolPackage = pkgs: tool:
    if tool == null || pkgs.lib.isDerivation tool
    then tool
    else tool.packages.${pkgs.stdenv.hostPlatform.system}.default;

  app = program: {
    type = "app";
    program = toString program;
  };

  mkDevShells = {
    pkgs,
    cross ? null,
    checks ? {},
    craneLib ? null,
    crossCraneLib ? null,
    x11 ? false,
    visualTest ? pkgs.stdenv.hostPlatform.isLinux,
    visualTestProfile ? "mesa-radv",
    fragpipe ? null,
    steampipe ? null,
    android ? null,
    windowsMsvc ? false,
    steam ? null,
    extraPackages ? [],
    extraPkgConfigDeps ? [],
    extraEnv ? {},
    shellHook ? "",
    ...
  }: let
    inherit (pkgs) lib;
    deps = bevyDeps {inherit pkgs x11;};
    visual =
      if visualTest
      then
        mkVisualTestEnv {
          inherit pkgs;
          profile = visualTestProfile;
        }
      else {
        packages = [];
        env = {};
      };
    crossEnv =
      if cross == null
      then defaultCross pkgs
      else cross;
    nativeCrane =
      if craneLib == null
      then (stableToolchain pkgs []).craneLib
      else craneLib;
    targetCrane =
      if crossCraneLib == null
      then
        (stableToolchain pkgs ([
            "x86_64-pc-windows-gnu"
          ]
          ++ lib.optionals ((crossEnv.osxcrossRustHelpers or null) != null) [
            "x86_64-apple-darwin"
            "aarch64-apple-darwin"
          ])).craneLib
      else crossCraneLib;
    packages =
      lib.optionals pkgs.stdenv.hostPlatform.isLinux deps.buildInputs
      ++ visual.packages
      ++ builtins.filter (p: p != null) (map (toolPackage pkgs) [fragpipe steampipe])
      ++ extraPackages;
    env =
      lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {LD_LIBRARY_PATH = deps.ldLibraryPath;}
      // visual.env // extraEnv;
    baseArgs = {
      inherit pkgs checks packages;
      cross = crossEnv;
      pkgConfigDeps = lib.optionals pkgs.stdenv.hostPlatform.isLinux deps.pkgConfigDeps ++ extraPkgConfigDeps;
      extraEnv = env;
      extraShellHook = shellHook;
      opencodeLsp.enable = false;
    };
    native = (harbor-rs.lib.mkDevShells (baseArgs // {craneLib = nativeCrane;})).default;
    crossShells = harbor-rs.lib.mkDevShells (baseArgs // {craneLib = targetCrane;});
    steamTools = mkSteamTools pkgs steam;
  in
    crossShells
    // {default = native;}
    // lib.optionalAttrs windowsMsvc {
      windows-msvc = harbor-rs.lib.mkWindowsMsvcDevShell {
        inherit pkgs lib checks;
        llvmPackages = pkgs.llvmPackages;
        toolchain = stableToolchain pkgs ["x86_64-pc-windows-msvc"];
        extraPackages = packages;
        extraEnv = env;
        extraShellHook = shellHook;
      };
    }
    // lib.optionalAttrs (android != null) {
      android = android.mkDevShell {base = native;};
    }
    // lib.optionalAttrs (steam != null) {
      steam = native.overrideAttrs (old: {
        nativeBuildInputs = (old.nativeBuildInputs or []) ++ builtins.attrValues steamTools.packages;
        shellHook = (old.shellHook or "") + steamTools.steamworksRsCargoLibraryHook;
      });
    };

  mkSteamTools = pkgs: args:
    harbor-rs.lib.mkSteamRuntimeTools ({
        inherit pkgs;
        rsHarborCli = harbor-rs.packages.${pkgs.stdenv.hostPlatform.system}.harbor-rs;
      }
      // args);

  mkAndroid = import ./android.nix {inherit harbor-android stableToolchain toolPackage app;};

  # steampipeToml selects build-time configuration. The upstream CLI still
  # discovers steampipe.toml in the working checkout when a runner is launched.
  mkTestCluster = {
    pkgs,
    nixpkgs,
    steampipe,
    projectRoot,
    steampipeToml ? projectRoot + "/steampipe.toml",
    clusterArgs ? {},
  }: let
    cluster = steampipe.lib.mkTestCluster (clusterArgs
      // {
        inherit pkgs nixpkgs steampipe projectRoot;
        inherit (pkgs) lib;
        projectConfig = builtins.fromTOML (builtins.readFile steampipeToml);
      });
  in
    cluster // {apps = pkgs.lib.mapAttrs (_: app) cluster.scripts;};

  mkBevyProject = import ./project.nix {
    inherit harbor-rs bevyDeps stableToolchain defaultCross mkDevShells mkAndroid mkTestCluster mkSteamTools app;
  };

  # project receives the per-system pkgs, including any consumer overlays or license options.
  mkBevyFlake = {
    inputs,
    systems ? ["x86_64-linux" "aarch64-linux"],
    project,
    treefmtModule ? {},
    treefmt-nix ? harbor-meta.inputs.treefmt-nix,
    imports ? [],
  }:
    inputs.flake-parts.lib.mkFlake {inherit inputs;} {
      inherit systems imports;
      perSystem = {
        pkgs,
        system,
        ...
      }: let
        result = mkBevyProject ({inherit pkgs;} // project {inherit pkgs system;});
        format = treefmt-nix.lib.evalModule pkgs {
          imports = [
            (import ../treefmt.nix {
              inherit harbor-rs harbor-meta;
              rustfmtPackage = result.toolchain.rustToolchain;
            })
            treefmtModule
          ];
        };
      in {
        inherit (result) apps packages devShells;
        formatter = format.config.build.wrapper;
        checks = result.checks // {formatting = format.config.build.check inputs.self;};
      };
    };
in {
  inherit bevyDeps mkVisualTestEnv mkDevShells mkAndroid mkTestCluster mkBevyProject mkBevyFlake;
}
