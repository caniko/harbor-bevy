# harbor-bevy-diagnostics

Structured logs, batch reports, counters, event audits, state hashes, and local
crash bundles. No feature flags are required for the standalone primitives.

- `bevy`: enables `DiagnosticsPlugin`, `CrashDumpPlugin`, and a capture layer
  for Bevy's `LogPlugin`. Count selected events with `EventAuditState::record`
  and refresh `HashTarget` whenever the caller-owned state it represents changes.
- `minidumper`: adds opt-in native crash capture. Dispatch
  `run_crash_reporter_process_if_requested` before application argument parsing,
  using the same crash root and reporter flag passed to the parent. Keep the
  `CrashDumpSession` alive after calling `start_minidumper`.

Create a `CrashDumpSession` with a dedicated local output directory. To include
application state, pass a snapshot function to `CrashDumpPlugin::new`, or update
the session directly without Bevy. Panic hooks and global tracing are explicit,
process-wide setup; install them once at startup. When using Bevy's `LogPlugin`,
attach the session capture layer there rather than installing a second global
tracing subscriber.

All artifacts remain local. Logs and memory dumps may contain sensitive data.
Only snapshot state that is safe to persist, use a private output directory,
and apply retention outside active sessions.
