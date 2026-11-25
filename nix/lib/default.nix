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
    env = pin.env // extraEnv;
  };

  mkDevShells = {
    pkgs,
    cross ? null,
    checks ? {},
    craneLib ? null,
    x11 ? false,
    visualTest ? true,
    visualTestProfile ? "mesa-radv",
    fragpipe ? null,
    steampipe ? null,
    extraPackages ? [],
    extraPkgConfigDeps ? [],
    extraEnv ? {},
    shellHook ? "",
    ...
  }: let
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
    tools = harbor-rs.lib.mkToolchain {
      inherit pkgs;
      toolchainProfile = "stable";
      targets = [];
    };
  in
    harbor-rs.lib.mkDevShells {
      inherit pkgs checks;
      craneLib =
        if craneLib == null
        then tools.craneLib
        else craneLib;
      cross =
        if cross == null
        then
          harbor-rs.lib.mkCross {
            inherit pkgs;
            inherit (pkgs.stdenv.hostPlatform) system;
          }
        else cross;
      packages = deps.buildInputs ++ visual.packages ++ builtins.filter (p: p != null) [fragpipe steampipe] ++ extraPackages;
      pkgConfigDeps = deps.pkgConfigDeps ++ extraPkgConfigDeps;
      extraEnv = {LD_LIBRARY_PATH = deps.ldLibraryPath;} // visual.env // extraEnv;
      extraShellHook = shellHook;
      opencodeLsp.enable = false;
    };
in {
  inherit bevyDeps mkVisualTestEnv mkDevShells;
}
