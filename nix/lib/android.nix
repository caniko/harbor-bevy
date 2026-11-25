{
  harbor-android,
  stableToolchain,
  toolPackage,
  app,
}: {
  pkgs,
  workspaceSrc,
  flavors,
  ndkVersion ? "29.0.14206865",
  androidSdk ? null,
  sdkArgs ? {},
  toolchain ? stableToolchain pkgs ["aarch64-linux-android" "armv7-linux-androideabi" "i686-linux-android" "x86_64-linux-android"],
  flavorArgs ? {},
  mavenCacheTar ? null,
  mavenCacheHostPath ? builtins.getEnv "MAVEN_CACHE_TAR",
  mavenCacheSha256Path ? null,
  fragpipe ? null,
  fragpipeToml ? "fragpipe.toml",
  androidShell ? ".#android",
}: let
  inherit (pkgs) lib;
  sdk =
    if androidSdk != null
    then androidSdk
    else
      (harbor-android.lib.mkAndroidSdk ({
          inherit pkgs;
          ndkVersions = [ndkVersion];
          includeNDK = true;
        }
        // sdkArgs)).androidsdk;
  cache =
    if mavenCacheTar != null
    then mavenCacheTar
    else if mavenCacheHostPath == null || mavenCacheHostPath == ""
    then null
    else
      assert lib.assertMsg (mavenCacheSha256Path != null) "MAVEN_CACHE_TAR requires mavenCacheSha256Path";
        harbor-android.lib.findLocalMavenCache {
          inherit lib;
          hostPath = mavenCacheHostPath;
          sha256Path = mavenCacheSha256Path;
          name = "android-maven-cache.tar";
          recursive = false;
        };
  table = harbor-android.lib.mkAndroidFlavorTable ({
      inherit pkgs ndkVersion workspaceSrc flavors;
      inherit (toolchain) rustToolchain;
      androidSdk = sdk;
      mavenCacheTar = cache;
      cargoVendorDir =
        if cache == null
        then null
        else toolchain.craneLib.vendorCargoDeps {src = workspaceSrc;};
    }
    // flavorArgs);
  androidApp = name: subcommand:
    app (pkgs.writeShellScript name ''
      config=(--config ${lib.escapeShellArg (toString fragpipeToml)})
      for arg in "$@"; do
        case "$arg" in --config|--config=*) config=(); break ;; esac
      done
      exec ${lib.getExe pkgs.nix} develop ${lib.escapeShellArg androidShell} -c \
        ${lib.getExe' (toolPackage pkgs fragpipe) "fragpipe"} ${subcommand} "''${config[@]}" "$@"
    '');
in
  table
  // {
    androidSdk = sdk;
    mavenCacheTar = cache;
    mkDevShell = args:
      harbor-android.lib.mkAndroidDevShell ({
          inherit pkgs ndkVersion;
          inherit (toolchain) rustToolchain;
          androidSdk = sdk;
        }
        // args);
    apps =
      table.apps
      // lib.optionalAttrs (fragpipe != null) {
        android-doctor = androidApp "android-doctor" "android-doctor";
        android-1v1-test = androidApp "android-1v1-test" "android-1v1";
        android-ui-test = androidApp "android-ui-test" "android-ui";
      };
  }
