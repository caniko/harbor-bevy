# platform-attestation

Steam and Play assertion verification with no game-engine or platform SDK dependency.

Use `AppBinding` for the Steam app ID. For Play, pass an `AttestationPolicy`
to `play_request_hash` and `verify_play`. Set the package to the distributed
app's ID and use the same hash-domain string on the client and server. The
domain is hashed byte-for-byte; changing it invalidates existing clients'
hashes.
