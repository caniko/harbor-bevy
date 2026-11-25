{
  config,
  lib,
  ...
}: let
  cfg = config.services.thespan-rendezvous;
  inherit (lib) mkOption types;
  str = default: description:
    mkOption {
      inherit default description;
      type = types.str;
    };
  limit = default: description:
    mkOption {
      inherit default description;
      type = types.ints.positive;
    };
  listenPorts = protocol: addrs:
    lib.unique (lib.concatMap (addr: let
      match = builtins.match ".*/${protocol}/([0-9]+)(/.*)?" addr;
    in
      lib.optional (match != null) (lib.toIntBase10 (builtins.head match)))
    addrs);
in {
  options.services.thespan-rendezvous = {
    enable = lib.mkEnableOption "the libp2p rendezvous and relay server";
    package = mkOption {
      type = types.package;
      description = "Package providing bin/thespan-rendezvous.";
    };
    stateDir = mkOption {
      type = types.strMatching "[A-Za-z0-9_][A-Za-z0-9_.-]*";
      default = "thespan-rendezvous";
      description = "Persistent systemd state directory name under /var/lib.";
    };
    keyPath =
      str "/var/lib/${cfg.stateDir}/identity.key"
      "Persistent libp2p identity key path; losing this key changes the server PeerId.";
    listenQuic = str "/ip4/0.0.0.0/udp/4001/quic-v1" "IPv4 QUIC listen multiaddress.";
    listenTcp = str "/ip4/0.0.0.0/tcp/4001" "IPv4 TCP listen multiaddress.";
    listenQuicV6 = str "/ip6/::/udp/4001/quic-v1" "IPv6 QUIC listen multiaddress.";
    listenTcpV6 = str "/ip6/::/tcp/4001" "IPv6 TCP listen multiaddress.";
    disableIpv6 = mkOption {
      type = types.bool;
      default = false;
      description = "Disable IPv6 listeners.";
    };
    externalAddr = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = "Externally advertised multiaddress, if different from the listen addresses.";
    };
    metricsAddr = str "127.0.0.1:9617" "Prometheus metrics bind address.";
    maxRegistrations = limit 50000 "Maximum live registrations.";
    rateLimitPerSec = limit 20 "Maximum requests per second per source IP.";
    clockSkewSecs = limit 60 "Accepted clock skew in seconds.";
    replayWindowSecs = limit 120 "Replay protection window in seconds.";
    relayMaxReservations = limit 1024 "Maximum live relay reservations.";
    relayMaxReservationsPerPeer = limit 4 "Maximum live relay reservations per peer.";
    relayReservationSecs = limit 7200 "Relay reservation lifetime in seconds.";
    relayMaxCircuits = limit 256 "Maximum concurrent relay circuits.";
    relayMaxCircuitsPerPeer = limit 8 "Maximum concurrent relay circuits per source peer.";
    relayCircuitSecs = limit 1800 "Maximum relay circuit lifetime in seconds.";
    relayCircuitBytes = limit 67108864 "Maximum bytes per relay circuit.";
    openFirewall = mkOption {
      type = types.bool;
      default = false;
      description = "Open the configured rendezvous TCP and QUIC ports in the firewall.";
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.services.thespan-rendezvous = {
      description = "libp2p rendezvous and relay server";
      after = ["network-online.target"];
      wants = ["network-online.target"];
      wantedBy = ["multi-user.target"];
      serviceConfig = {
        ExecStart = "${cfg.package}/bin/thespan-rendezvous";
        Restart = "on-failure";
        RestartSec = "3s";
        DynamicUser = true;
        StateDirectory = cfg.stateDir;
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
        MemoryDenyWriteExecute = true;
        SystemCallArchitectures = "native";
        SystemCallFilter = ["@system-service" "~@privileged @resources"];
        MemoryMax = "256M";
        TasksMax = 512;
      };
      environment =
        {
          THESPAN_RDV_KEY = cfg.keyPath;
          THESPAN_RDV_QUIC = cfg.listenQuic;
          THESPAN_RDV_TCP = cfg.listenTcp;
          THESPAN_RDV_QUIC_V6 = cfg.listenQuicV6;
          THESPAN_RDV_TCP_V6 = cfg.listenTcpV6;
          THESPAN_RDV_DISABLE_IPV6 = lib.boolToString cfg.disableIpv6;
          THESPAN_RDV_LOG_JSON = "true";
          THESPAN_RDV_METRICS_ADDR = cfg.metricsAddr;
          THESPAN_RDV_MAX_REGS = toString cfg.maxRegistrations;
          THESPAN_RDV_RATE = toString cfg.rateLimitPerSec;
          THESPAN_RDV_SKEW = toString cfg.clockSkewSecs;
          THESPAN_RDV_REPLAY = toString cfg.replayWindowSecs;
          THESPAN_RDV_RELAY_MAX_RESERVATIONS = toString cfg.relayMaxReservations;
          THESPAN_RDV_RELAY_MAX_RESERVATIONS_PER_PEER = toString cfg.relayMaxReservationsPerPeer;
          THESPAN_RDV_RELAY_RESERVATION_SECS = toString cfg.relayReservationSecs;
          THESPAN_RDV_RELAY_MAX_CIRCUITS = toString cfg.relayMaxCircuits;
          THESPAN_RDV_RELAY_MAX_CIRCUITS_PER_PEER = toString cfg.relayMaxCircuitsPerPeer;
          THESPAN_RDV_RELAY_CIRCUIT_SECS = toString cfg.relayCircuitSecs;
          THESPAN_RDV_RELAY_CIRCUIT_BYTES = toString cfg.relayCircuitBytes;
          RUST_LOG = "info,libp2p_swarm=warn";
        }
        // lib.optionalAttrs (cfg.externalAddr != null) {
          THESPAN_RDV_EXT = cfg.externalAddr;
        };
    };
    networking.firewall = lib.mkIf cfg.openFirewall {
      allowedTCPPorts = listenPorts "tcp" ([cfg.listenTcp] ++ lib.optional (!cfg.disableIpv6) cfg.listenTcpV6);
      allowedUDPPorts = listenPorts "udp" ([cfg.listenQuic] ++ lib.optional (!cfg.disableIpv6) cfg.listenQuicV6);
    };
  };
}
