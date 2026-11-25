//! Derive macros for the `bevy_replicated` replication substrate.
//!
//! Provides `#[derive(Replicated)]` for a component type. It generates the
//! `bevy::ecs::component::Component` impl (mediated-access, table storage) plus
//! the `bevy_replicated::ReplicatedComponent` impl, whose `encode_payload`/
//! `decode_payload` round-trip the component through rkyv.
//!
//! The required `#[replicated(component_id = N)]` attribute assigns the
//! component's stable wire id (`ComponentId(N)`). This id is the on-the-wire and
//! on-disk identity of the component across the replication substrate, so it must
//! stay stable for the life of the component — treat the id space as an
//! append-only registry; never renumber or reuse a retired id. Omitting the
//! attribute is a compile error.

use proc_macro::TokenStream;
use quote::quote;
use syn::{DeriveInput, LitInt, parse_macro_input};

#[proc_macro_derive(Replicated, attributes(component, replicated))]
pub fn derive_replicated(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let component_id = match stable_id(&input, "component_id") {
        Ok(Some(component_id)) => component_id,
        Ok(None) => {
            return syn::Error::new_spanned(
                &input.ident,
                "missing #[replicated(component_id = N)] stable component id",
            )
            .into_compile_error()
            .into();
        }
        Err(error) => return error.into_compile_error().into(),
    };

    let ident = &input.ident;
    let (impl_generics, type_generics, where_clause) = input.generics.split_for_impl();

    quote! {
        impl #impl_generics ::bevy::ecs::component::Component for #ident #type_generics #where_clause {
            const STORAGE_TYPE: ::bevy::ecs::component::StorageType =
                ::bevy::ecs::component::StorageType::Table;
            const MEDIATED_ACCESS: bool = true;
            type Mutability = ::bevy::ecs::component::MediatedMutable;
        }

        impl #impl_generics ::bevy::ecs::component::MediatedAccess for #ident #type_generics #where_clause {
        }

        impl #impl_generics ::bevy_replicated::ReplicatedComponent for #ident #type_generics #where_clause {
            const COMPONENT_ID: ::bevy_replicated::ComponentId =
                ::bevy_replicated::ComponentId(#component_id);

            fn encode_payload(&self) -> Result<Vec<u8>, ::bevy_replicated::WireError> {
                ::rkyv::to_bytes::<::rkyv::rancor::Error>(self)
                    .map(|bytes| bytes.to_vec())
                    .map_err(::bevy_replicated::WireError::Encode)
            }

            fn decode_payload(bytes: &[u8]) -> Result<Self, ::bevy_replicated::WireError> {
                ::rkyv::from_bytes::<Self, ::rkyv::rancor::Error>(bytes)
                    .map_err(::bevy_replicated::WireError::Decode)
            }
        }
    }
    .into()
}

#[proc_macro_derive(ReplicatedResource, attributes(replicated))]
pub fn derive_replicated_resource(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let resource_id = match stable_id(&input, "resource_id") {
        Ok(Some(resource_id)) => resource_id,
        Ok(None) => {
            return syn::Error::new_spanned(
                &input.ident,
                "missing #[replicated(resource_id = N)] stable resource id",
            )
            .into_compile_error()
            .into();
        }
        Err(error) => return error.into_compile_error().into(),
    };

    let ident = &input.ident;
    let (impl_generics, type_generics, where_clause) = input.generics.split_for_impl();

    quote! {
        impl #impl_generics ::bevy::ecs::component::Component for #ident #type_generics #where_clause {
            const STORAGE_TYPE: ::bevy::ecs::component::StorageType =
                ::bevy::ecs::component::StorageType::SparseSet;
            type Mutability = ::bevy::ecs::component::Mutable;

            fn register_required_components(
                _component_id: ::bevy::ecs::component::ComponentId,
                required_components: &mut ::bevy::ecs::component::RequiredComponentsRegistrator,
            ) {
                let resource_component_id = if let ::std::option::Option::Some(id) =
                    required_components
                        .components_registrator()
                        .component_id::<#ident #type_generics>()
                {
                    id
                } else {
                    required_components
                        .components_registrator()
                        .register_component::<#ident #type_generics>()
                };
                required_components.register_required::<::bevy::ecs::resource::IsResource>(
                    move || ::bevy::ecs::resource::IsResource::new(resource_component_id),
                );
            }
        }

        impl #impl_generics ::bevy::prelude::Resource for #ident #type_generics #where_clause {
        }

        impl #impl_generics ::bevy_replicated::ReplicatedResource for #ident #type_generics #where_clause {
            const RESOURCE_ID: ::bevy_replicated::ResourceId =
                ::bevy_replicated::ResourceId(#resource_id);

            fn encode_payload(&self) -> Result<Vec<u8>, ::bevy_replicated::WireError> {
                ::rkyv::to_bytes::<::rkyv::rancor::Error>(self)
                    .map(|bytes| bytes.to_vec())
                    .map_err(::bevy_replicated::WireError::Encode)
            }

            fn decode_payload(bytes: &[u8]) -> Result<Self, ::bevy_replicated::WireError> {
                ::rkyv::from_bytes::<Self, ::rkyv::rancor::Error>(bytes)
                    .map_err(::bevy_replicated::WireError::Decode)
            }
        }
    }
    .into()
}

fn stable_id(input: &DeriveInput, expected_key: &str) -> syn::Result<Option<u32>> {
    let mut found = None;
    for attr in &input.attrs {
        if !attr.path().is_ident("replicated") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident(expected_key) {
                let value = meta.value()?;
                let lit: LitInt = value.parse()?;
                found = Some(lit.base10_parse::<u32>()?);
                Ok(())
            } else {
                Err(meta.error("unsupported replicated attribute"))
            }
        })?;
    }
    Ok(found)
}
