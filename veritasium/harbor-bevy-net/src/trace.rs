pub enum Field<'a> {
    MessageKind(&'a str),
    MessageTurn(u32),
    CurrentTurn(u32),
    LocalPlayer(u8),
}

pub fn record_field(span: &tracing::Span, field: Field<'_>) {
    match field {
        Field::MessageKind(value) => span.record("message_kind", value),
        Field::MessageTurn(value) => span.record("message_turn", value),
        Field::CurrentTurn(value) => span.record("current_turn", value),
        Field::LocalPlayer(value) => span.record("local_player", value),
    };
}

#[macro_export]
macro_rules! span_record {
    ($span:expr, $($field:expr),+ $(,)?) => {{
        let span = &$span;
        $($crate::trace::record_field(span, $field);)+
    }};
}

pub const RECONNECT_ATTEMPTS_TOTAL: &str = "net_reconnect_attempts_total";
pub const RECONNECT_LATENCY_SECONDS: &str = "net_reconnect_latency_seconds";
pub const REJOIN_ATTEMPTS_TOTAL: &str = "net_rejoin_attempts_total";
pub const REJOIN_LATENCY_SECONDS: &str = "net_rejoin_latency_seconds";

pub fn recovery_latency_bucket(seconds: f64) -> &'static str {
    [
        (0.1, "0.1"),
        (0.5, "0.5"),
        (1., "1"),
        (2., "2"),
        (5., "5"),
        (10., "10"),
        (30., "30"),
    ]
    .into_iter()
    .find(|(limit, _)| seconds <= *limit)
    .map_or("gt_30", |(_, label)| label)
}
