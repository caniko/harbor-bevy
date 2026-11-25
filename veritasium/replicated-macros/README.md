# bevy_replicated_macros

Procedural macros for [`bevy_replicated`](..).

This crate provides the `#[derive(Replicated)]` macro used by
`bevy_replicated` components. It is published and versioned separately only
because Rust procedural macros must live in their own crate; application code
should normally depend on `bevy_replicated` and import the derive from there.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
