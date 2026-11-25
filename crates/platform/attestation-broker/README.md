# attestation-broker

Reusable attestation broker library. The `bin` feature enables the HTTP server.

## Play attestation

Flow: the client calls `POST /v1/attest/play/nonce` (single use, 5-minute
TTL), runs Play Age Signals, and requests a Standard Integrity token with
`requestHash = base64url(SHA-256(domain + "\n" + nonce + "\n" + claim))`
(unpadded). It then posts `{token, claim, nonce}` to `POST /v1/attest/play`. The
broker spends the nonce, has Google decode the token, and checks the package,
hash, verdict age (120 s), `PLAY_RECOGNIZED`, `MEETS_DEVICE_INTEGRITY` and
`LICENSED` before mapping the claim to a protection class. Responses carry
`protection` next to `token`, `exp` and `scopes`.

Configuration:

- `--play-package` / `ATTESTATION_PLAY_PACKAGE`: required Android package ID
  (for example, `com.example.game`).
- `--play-age-hash-domain` / `ATTESTATION_PLAY_AGE_HASH_DOMAIN`: required hash
  domain. Configure the client's existing value byte-for-byte; there is no default.
- `PLAY_INTEGRITY_SERVICE_ACCOUNT_FILE`: path to the Google service-account
  key JSON, read once at startup. If it is unset, unreadable or invalid, both
  Play routes answer 503 and every other route keeps serving. Never commit the
  file; deploy it as a secret.
- `--play-adult-age-sources` (default `TIER_C,TIER_D`): Age Signals sources
  that may prove an adult. Self-declared (`TIER_A`) and parent-managed
  (`TIER_B`) adults stay Unknown unless the operator widens this.
- `STEAM_WEB_API_KEY(_FILE)`: if absent, `/v1/attest/steam` answers 503
  instead of the broker refusing to start.
- `--client-ip-header NAME` (unset by default): header a trusted reverse proxy
  sets to the client address, for the per-address nonce limits (16 unspent,
  30 per minute, IPv6 per /64; 429 when exceeded). Unset, the socket peer is
  used and such headers are ignored.

Play Console and Google Cloud prerequisites (outside this repository):

1. Request Age Signals API access for the app in Play Console (beta; only
   apps distributed by Google Play).
2. Link a Google Cloud project to the app in Play Console (App integrity).
3. Enable the Play Integrity API in that Cloud project.
4. Create a service account in that project and download its JSON key for
   `PLAY_INTEGRITY_SERVICE_ACCOUNT_FILE` (the broker requests the
   `https://www.googleapis.com/auth/playintegrity` scope).
5. Configure the Android client with the Cloud project number, package ID,
   and matching age-hash domain.
