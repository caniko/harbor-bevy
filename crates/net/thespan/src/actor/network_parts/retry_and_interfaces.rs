fn jittered_delay(base: Duration, attempt: u8) -> Duration {
    let millis = u64::try_from(base.as_millis()).unwrap_or(u64::MAX);
    let jitter_span = (millis / 4).max(1);
    let offset = (u64::from(attempt) * 37) % (jitter_span * 2 + 1);
    let jittered = millis + offset - jitter_span;
    Duration::from_millis(jittered.max(1))
}
