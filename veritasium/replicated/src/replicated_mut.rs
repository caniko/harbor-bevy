use crate::{ComponentId, ResourceId, WireError};

/// Component contract for deterministic replicated payloads.
///
/// Implementors provide a stable component id plus wire payload encode/decode
/// hooks used by transaction capture and apply. The derive macro is the normal
/// path; manual implementations are useful when a component needs a custom
/// merge policy.
///
/// # Examples
///
/// ```
/// use bevy::prelude::Component;
/// use bevy_replicated::{ComponentId, ReplicatedComponent, WireError};
///
/// #[derive(Component, Clone, Debug, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
/// struct Health(u32);
///
/// impl ReplicatedComponent for Health {
///     const COMPONENT_ID: ComponentId = ComponentId(7);
///
///     fn encode_payload(&self) -> Result<Vec<u8>, WireError> {
///         rkyv::to_bytes::<rkyv::rancor::Error>(self)
///             .map(|bytes| bytes.to_vec())
///             .map_err(WireError::Encode)
///     }
///
///     fn decode_payload(bytes: &[u8]) -> Result<Self, WireError> {
///         rkyv::from_bytes::<Self, rkyv::rancor::Error>(bytes).map_err(WireError::Decode)
///     }
/// }
///
/// let bytes = Health(10).encode_payload().unwrap();
/// assert_eq!(Health::decode_payload(&bytes).unwrap(), Health(10));
/// ```
pub trait ReplicatedComponent:
    bevy::ecs::component::Component + Clone + Send + Sync + 'static
{
    /// Stable replicated component id.
    ///
    /// This id is folded into transaction bytes and must not be reused for a
    /// different component type inside the same replicated protocol.
    const COMPONENT_ID: ComponentId;

    /// Encode this component into deterministic transport payload bytes.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when the component cannot be encoded into the
    /// crate's stable wire payload.
    fn encode_payload(&self) -> Result<Vec<u8>, WireError>;

    /// Decode deterministic transport payload bytes into a component value.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when `bytes` are not a valid payload for this
    /// component type.
    fn decode_payload(bytes: &[u8]) -> Result<Self, WireError>
    where
        Self: Sized;

    /// Merge an incoming remote component value with the current local value.
    ///
    /// The default last-writer-wins behavior returns `incoming`. Override this
    /// only for deterministic, associative merge semantics that are part of the
    /// replicated protocol.
    fn merge_remote(current: Option<&Self>, incoming: Self) -> Self
    where
        Self: Sized,
    {
        let _ = current;
        incoming
    }
}

/// Resource contract for deterministic replicated singleton payloads.
///
/// Resource writes are captured only through `bevy_replicated`'s explicit
/// resource APIs. Bevy does not currently expose a mediated resource write gate,
/// so ordinary `ResMut<T>` writes are intentionally outside this capture model.
pub trait ReplicatedResource:
    bevy::prelude::Resource<Mutability = bevy::ecs::component::Mutable> + Clone + Send + Sync + 'static
{
    /// Stable replicated resource id.
    ///
    /// This id is folded into transaction bytes and must not be reused for a
    /// different resource type inside the same replicated protocol.
    const RESOURCE_ID: ResourceId;

    /// Encode this resource into deterministic transport payload bytes.
    fn encode_payload(&self) -> Result<Vec<u8>, WireError>;

    /// Decode deterministic transport payload bytes into a resource value.
    fn decode_payload(bytes: &[u8]) -> Result<Self, WireError>
    where
        Self: Sized;

    /// Merge an incoming remote resource value with the current local value.
    fn merge_remote(current: Option<&Self>, incoming: Self) -> Self
    where
        Self: Sized,
    {
        let _ = current;
        incoming
    }
}
