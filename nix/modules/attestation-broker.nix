{
  config,
  lib,
  utils,
  ...
}: let
  cfg = config.services.attestation-broker;
  inherit (lib) mkOption types;
  credential = types.strMatching "/[^\n\r]*";
  credentials = {
    token-hmac-key = cfg.tokenHmacKeyFile;
    detritus-token = cfg.detritusTokenFile;
    steam-web-api-key = cfg.steamWebApiKeyFile;
    play-integrity-service-account = cfg.playIntegrityServiceAccountFile;
  };
in {
  options.services.attestation-broker = {
    enable = lib.mkEnableOption "the platform attestation broker";
    package = mkOption {
      type = types.package;
      description = "Package providing bin/attestation-broker.";
    };
    bind = mkOption {
      type = types.nonEmptyStr;
      default = "127.0.0.1:8080";
      description = "HTTP bind address and port.";
    };
    steamAppId = mkOption {
      type = types.ints.positive;
      description = "Steam App ID accepted by the broker.";
    };
    playPackage = mkOption {
      type = types.nonEmptyStr;
      description = "Android package name accepted by Play Integrity verification.";
    };
    playAgeHashDomain = mkOption {
      type = types.nonEmptyStr;
      description = "Client-matching domain separator for the Play age-attestation request hash.";
    };
    steamWebApiKeyFile = mkOption {
      type = types.nullOr credential;
      default = null;
      description = "Runtime credential file for the Steam publisher Web API key; null disables Steam verification.";
    };
    playIntegrityServiceAccountFile = mkOption {
      type = types.nullOr credential;
      default = null;
      description = "Runtime credential file for the Play Integrity service account; null disables Play verification.";
    };
    tokenHmacKeyFile = mkOption {
      type = credential;
      description = "Runtime credential file for the broker's token HMAC key.";
    };
    detritusTokenFile = mkOption {
      type = credential;
      description = "Runtime credential file for the telemetry upstream token.";
    };
    detritusUrl = mkOption {
      type = types.nonEmptyStr;
      default = "http://127.0.0.1:4317";
      description = "Telemetry upstream URL.";
    };
    stateDir = mkOption {
      type = types.strMatching "[A-Za-z0-9_][A-Za-z0-9_.-]*";
      default = "attestation-broker";
      description = "Persistent systemd state directory name under /var/lib.";
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.services.attestation-broker = {
      description = "Platform attestation broker";
      after = ["network-online.target"];
      wants = ["network-online.target"];
      wantedBy = ["multi-user.target"];
      environment =
        {
          TOKEN_HMAC_KEY_FILE = "%d/token-hmac-key";
          DETRITUS_TOKEN_FILE = "%d/detritus-token";
        }
        // lib.optionalAttrs (cfg.steamWebApiKeyFile != null) {
          STEAM_WEB_API_KEY_FILE = "%d/steam-web-api-key";
        }
        // lib.optionalAttrs (cfg.playIntegrityServiceAccountFile != null) {
          PLAY_INTEGRITY_SERVICE_ACCOUNT_FILE = "%d/play-integrity-service-account";
        };
      serviceConfig = {
        ExecStart = utils.escapeSystemdExecArgs [
          "${cfg.package}/bin/attestation-broker"
          "--bind"
          cfg.bind
          "--steam-app-id"
          cfg.steamAppId
          "--play-package"
          cfg.playPackage
          "--play-age-hash-domain"
          cfg.playAgeHashDomain
          "--detritus-url"
          cfg.detritusUrl
          "--data-dir"
          "/var/lib/${cfg.stateDir}"
        ];
        LoadCredential =
          lib.mapAttrsToList
          (name: path: "${name}:${lib.replaceStrings ["%"] ["%%"] path}")
          (lib.filterAttrs (_: path: path != null) credentials);
        DynamicUser = true;
        StateDirectory = cfg.stateDir;
        Restart = "on-failure";
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        NoNewPrivileges = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectControlGroups = true;
        RestrictNamespaces = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
        LockPersonality = true;
      };
    };
  };
}
