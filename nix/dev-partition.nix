{
  inputs,
  self,
  ...
}: {
  perSystem = {
    pkgs,
    system,
    ...
  }: let
    inherit (pkgs) lib;
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
    fixtureScript = pkgs.writeShellScript "fixture" "exit 0";
    fixtureSteampipe = {
      packages.${system}.default = pkgs.hello;
      lib.mkTestCluster = {projectConfig, ...}:
        assert projectConfig.cluster.graphics == false; {
          scripts.cluster-fixture = fixtureScript;
        };
    };
    clusterSettings = {
      inherit (inputs) nixpkgs;
      projectRoot = self.outPath;
      steampipeToml = builtins.toFile "test-cluster.toml" ''
        [cluster]
        graphics = false
      '';
    };
    androidSettings = {
      androidSdk = pkgs.hello;
      toolchain = tools;
      mavenCacheHostPath = null;
      flavors.demo = {
        cargoPkg = "demo";
        packageModes = ["debug"];
      };
    };
    projectSettings = {
      src = ./lib/fixtures/project;
      pname = "fixture";
      version = "0.1.0";
      binary = "example";
      toolchain = tools;
      fragpipe = pkgs.hello;
      steampipe = fixtureSteampipe;
      android = androidSettings;
      cluster = clusterSettings;
      crossTargets = ["darwin-x86_64"];
      steam.rsHarborCli = pkgs.hello;
      shellArgs.crossCraneLib = tools.craneLib;
    };
    project = self.lib.mkBevyProject ({inherit pkgs;} // projectSettings);
    fixtureFlake = self.lib.mkBevyFlake {
      inherit inputs;
      systems = [system];
      project = _: projectSettings;
    };
    fixtureOS = pkgs.nixos {
      imports = builtins.attrValues self.nixosModules;
      services.thespan-rendezvous = {
        enable = true;
        package = pkgs.hello;
        stateDir = "test-rendezvous";
        externalAddr = "/dns4/relay.example.invalid/tcp/4110";
        listenTcp = "/ip4/0.0.0.0/tcp/4110";
        listenQuic = "/ip4/0.0.0.0/udp/4111/quic-v1";
        disableIpv6 = true;
        openFirewall = true;
      };
      services.attestation-broker = {
        enable = true;
        package = pkgs.hello;
        steamAppId = 12345;
        playPackage = "org.example.demo";
        playAgeHashDomain = "demo-age-v1";
        tokenHmacKeyFile = "/run/secrets/hmac";
        detritusTokenFile = "/run/secrets/upstream";
        playIntegrityServiceAccountFile = "/run/secrets/play";
      };
    };
    rendezvous = fixtureOS.config.systemd.services.thespan-rendezvous;
    broker = fixtureOS.config.systemd.services.attestation-broker;
    platformPkgs = import inputs.nixpkgs {
      inherit system;
      config = {
        allowUnfree = true;
        microsoftVisualStudioLicenseAccepted = true;
      };
    };
  in {
    formatter = treefmt.config.build.wrapper;
    devShells.default =
      (self.lib.mkDevShells {
        inherit pkgs;
        inherit (tools) craneLib;
        extraPackages = [pkgs.python3 treefmt.config.build.wrapper];
        shellHook = lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
          for _hb_steam_dir in "''${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/steamworks-sys-*/lib/steam/redistributable_bin/linux64; do
            if [ -f "$_hb_steam_dir/libsteam_api.so" ]; then
              export LD_LIBRARY_PATH="$_hb_steam_dir''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
              break
            fi
          done
          unset _hb_steam_dir
        '';
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
      assert visual.env.HARBOR_BEVY_REQUIRE_HARDWARE_VULKAN == "1";
      assert visual.env.WGPU_BACKEND == "vulkan";
      assert pkgs.lib.hasSuffix ".json" visual.env.VK_DRIVER_FILES;
      assert lib.isDerivation project.packages.default;
      assert lib.hasSuffix "/bin/example" project.apps.default.program;
      assert lib.isDerivation project.packages.fixture-darwin-x86_64;
      assert lib.isDerivation project.devShells.default;
      assert lib.isDerivation project.devShells.cross;
      assert lib.isDerivation project.devShells.android;
      assert lib.isDerivation project.devShells.steam;
      assert system
      != "x86_64-linux"
      || lib.hasSuffix ".drv"
      (
        self.lib.mkDevShells {
          pkgs = platformPkgs;
          windowsMsvc = true;
          visualTest = false;
        }
      ).windows-msvc.drvPath;
      assert lib.hasSuffix "-android-doctor" project.apps.android-doctor.program;
      assert project.apps.cluster-fixture.program == toString fixtureScript;
      assert lib.isDerivation fixtureFlake.formatter.${system};
      assert lib.isDerivation fixtureFlake.checks.${system}.formatting;
        pkgs.writeText "harbor-bevy-lib-eval" "Bevy project, shell, Android, cluster and flake builders evaluate\n";
      modules-eval = assert rendezvous.serviceConfig.StateDirectory == "test-rendezvous";
      assert rendezvous.environment.THESPAN_RDV_KEY == "/var/lib/test-rendezvous/identity.key";
      assert rendezvous.environment.THESPAN_RDV_EXT == "/dns4/relay.example.invalid/tcp/4110";
      assert rendezvous.environment.THESPAN_RDV_DISABLE_IPV6 == "true";
      assert rendezvous.environment.THESPAN_RDV_MAX_REGS == "50000";
      assert lib.elem 4110 fixtureOS.config.networking.firewall.allowedTCPPorts;
      assert lib.elem 4111 fixtureOS.config.networking.firewall.allowedUDPPorts;
      assert !(lib.elem 4001 fixtureOS.config.networking.firewall.allowedTCPPorts);
      assert lib.hasInfix ''"--play-package" "org.example.demo"'' broker.serviceConfig.ExecStart;
      assert lib.hasInfix ''"--play-age-hash-domain" "demo-age-v1"'' broker.serviceConfig.ExecStart;
      assert broker.environment.TOKEN_HMAC_KEY_FILE == "%d/token-hmac-key";
      assert broker.environment.PLAY_INTEGRITY_SERVICE_ACCOUNT_FILE == "%d/play-integrity-service-account";
      assert !(broker.environment ? STEAM_WEB_API_KEY_FILE);
      assert lib.elem "token-hmac-key:/run/secrets/hmac" broker.serviceConfig.LoadCredential;
        pkgs.writeText "harbor-bevy-modules-eval" "Rendezvous and attestation service contracts evaluate\n";
    };
  };
}
