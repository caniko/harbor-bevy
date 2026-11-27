//! Bind a capture to verified source bytes and the image handles the renderer prepared.

use std::{collections::BTreeSet, fmt::Write};

use bevy::{
    asset::Handle,
    prelude::{App, Image, Resource},
    render::{
        RenderApp,
        extract_resource::ExtractResource,
        render_asset::RenderAssets,
        render_resource::{TextureDescriptor, TextureDimension, TextureFormat},
        renderer::RenderAdapterInfo,
        texture::GpuImage,
    },
};
use harbor_asset_pack::VerifiedAssetPack;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::gpu_pin::GpuAdapterIdentity;

const SCHEMA_VERSION: u32 = 1;

/// Declare a page using the same handle bound by the scene, not a second decode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageProvenanceReceipt {
    pub name: String,
    /// Lowercase SHA-256 hex of the original packed bytes.
    pub sha256: String,
    pub handle: Handle<Image>,
    pub dimensions: [u32; 2],
    pub mip_levels: u32,
    /// Declared maximum LOD; Bevy does not expose the prepared sampler descriptor.
    pub safe_lod: u32,
    pub format: TextureFormat,
}

/// Register Bevy's `ExtractResourcePlugin::<VisualImageProvenance>` after `RenderPlugin`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Resource, ExtractResource)]
#[extract_app(RenderApp)]
pub struct VisualImageProvenance(pub Vec<ImageProvenanceReceipt>);

/// Stable, serializable evidence. Runtime asset IDs are intentionally not persisted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedImageReceipt {
    pub name: String,
    pub sha256: String,
    pub dimensions: [u32; 2],
    pub mip_levels: u32,
    pub safe_lod: u32,
    pub format: String,
}

impl From<&ImageProvenanceReceipt> for PreparedImageReceipt {
    fn from(page: &ImageProvenanceReceipt) -> Self {
        Self {
            name: page.name.clone(),
            sha256: page.sha256.clone(),
            dimensions: page.dimensions,
            mip_levels: page.mip_levels,
            safe_lod: page.safe_lod,
            format: format!("{:?}", page.format),
        }
    }
}

#[derive(Debug, Error)]
pub enum ProvenanceError {
    #[error("main world has no image provenance resource")]
    MissingMainReceipts,
    #[error("render sub-app unavailable; use synchronous rendering for app-level verification")]
    MissingRenderApp,
    #[error("image provenance has not been extracted into the render world")]
    MissingExtractedReceipts,
    #[error("extracted image provenance differs from the current main-world receipts")]
    StaleReceipts,
    #[error("render world has no GPU image asset store")]
    MissingRenderImages,
    #[error("GPU image not prepared for {0}")]
    ImageNotPrepared(String),
    #[error("prepared GPU image descriptor or upload state differs for {0}")]
    ImageMismatch(String),
    #[error("verified pack has no matching source bytes for {0}")]
    SourceMismatch(String),
    #[error("renderer adapter identity is unavailable")]
    MissingAdapter,
    #[error("invalid capture provenance: {0}")]
    Invalid(&'static str),
    #[error("capture manifest differs from the expected provenance")]
    ManifestMismatch,
    #[error("invalid capture manifest JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// Verify live GPU image preparation after a frame has run. The check matches the
/// extracted handles and verifies the corresponding GPU descriptor and upload
/// state. It does not read back GPU memory or verify sampler parameters.
pub fn verify_prepared_gpu_images(app: &App) -> Result<Vec<PreparedImageReceipt>, ProvenanceError> {
    use ProvenanceError as Error;

    let expected = app
        .world()
        .get_resource::<VisualImageProvenance>()
        .ok_or(Error::MissingMainReceipts)?;
    let render = app
        .get_sub_app(RenderApp)
        .ok_or(Error::MissingRenderApp)?
        .world();
    let extracted = render
        .get_resource::<VisualImageProvenance>()
        .ok_or(Error::MissingExtractedReceipts)?;
    if expected != extracted {
        return Err(Error::StaleReceipts);
    }
    let mut pages: Vec<_> = expected.0.iter().map(PreparedImageReceipt::from).collect();
    pages.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    validate_pages(&pages)?;
    let images = render
        .get_resource::<RenderAssets<GpuImage>>()
        .ok_or(Error::MissingRenderImages)?;
    let mut ids = BTreeSet::new();
    for receipt in &extracted.0 {
        let id = receipt.handle.id();
        if !ids.insert(id) {
            return Err(Error::Invalid("duplicate image handle"));
        }
        let image = images
            .get(id)
            .ok_or_else(|| Error::ImageNotPrepared(receipt.name.clone()))?;
        verify_descriptor(receipt, &image.texture_descriptor, image.had_data)?;
    }
    Ok(pages)
}

fn verify_descriptor(
    receipt: &ImageProvenanceReceipt,
    desc: &TextureDescriptor<'_>,
    had_data: bool,
) -> Result<(), ProvenanceError> {
    if [desc.size.width, desc.size.height] != receipt.dimensions
        || desc.mip_level_count != receipt.mip_levels
        || desc.format != receipt.format
        || desc.dimension != TextureDimension::D2
        || desc.size.depth_or_array_layers != 1
        || desc.sample_count != 1
        || !had_data
    {
        return Err(ProvenanceError::ImageMismatch(receipt.name.clone()));
    }
    Ok(())
}

/// Versioned capture identity. Digests are lowercase hex; pages are sorted by name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureManifest {
    pub schema_version: u32,
    pub git_head: String,
    pub cargo_lock_sha256: String,
    /// SHA-256 of the asset pack's verified `SHA256SUMS`.
    pub asset_pack_sha256: String,
    pub adapter: GpuAdapterIdentity,
    pub prepared_pages: Vec<PreparedImageReceipt>,
}

impl CaptureManifest {
    /// Construct from trusted checkout, pack, adapter and page evidence.
    /// For a running capture, prefer [`Self::from_app`].
    pub fn new(
        git_head: &str,
        cargo_lock: &[u8],
        asset_pack_digest: [u8; 32],
        adapter: GpuAdapterIdentity,
        mut prepared_pages: Vec<PreparedImageReceipt>,
    ) -> Result<Self, ProvenanceError> {
        prepared_pages.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        let manifest = Self {
            schema_version: SCHEMA_VERSION,
            git_head: git_head.into(),
            cargo_lock_sha256: sha256(cargo_lock),
            asset_pack_sha256: hex(&asset_pack_digest),
            adapter,
            prepared_pages,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    /// Build capture evidence from the current app and verified pack. The caller
    /// supplies the commit ID and lockfile bytes used to build the capture.
    pub fn from_app(
        app: &App,
        git_head: &str,
        cargo_lock: &[u8],
        pack: &VerifiedAssetPack<'_>,
    ) -> Result<Self, ProvenanceError> {
        let prepared_pages = verify_prepared_gpu_images(app)?;
        for page in &prepared_pages {
            if pack
                .get(&page.name)
                .is_none_or(|bytes| sha256(bytes) != page.sha256)
            {
                return Err(ProvenanceError::SourceMismatch(page.name.clone()));
            }
        }
        let adapter = app
            .world()
            .get_resource::<RenderAdapterInfo>()
            .map(GpuAdapterIdentity::from)
            .ok_or(ProvenanceError::MissingAdapter)?;
        Self::new(
            git_head,
            cargo_lock,
            pack.manifest_digest(),
            adapter,
            prepared_pages,
        )
    }

    /// Validate the wire representation; this does not authenticate the evidence.
    pub fn validate(&self) -> Result<(), ProvenanceError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ProvenanceError::Invalid("unsupported schema version"));
        }
        if !matches!(self.git_head.len(), 40 | 64)
            || !lower_hex(&self.git_head)
            || !digest_hex(&self.cargo_lock_sha256)
            || !digest_hex(&self.asset_pack_sha256)
        {
            return Err(ProvenanceError::Invalid("invalid git HEAD or SHA-256"));
        }
        if ![
            &self.adapter.name,
            &self.adapter.backend,
            &self.adapter.device_type,
        ]
        .iter()
        .all(|s| !s.trim().is_empty() && !s.chars().any(char::is_control))
        {
            return Err(ProvenanceError::Invalid(
                "missing or invalid adapter identity",
            ));
        }
        validate_pages(&self.prepared_pages)
    }

    pub fn to_json(&self) -> Result<Vec<u8>, ProvenanceError> {
        self.validate()?;
        Ok(serde_json::to_vec_pretty(self)?)
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self, ProvenanceError> {
        let manifest: Self = serde_json::from_slice(bytes)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Compare all fields against independently obtained, trusted evidence.
    pub fn verify_against(&self, expected: &Self) -> Result<(), ProvenanceError> {
        self.validate()?;
        expected.validate()?;
        if self != expected {
            return Err(ProvenanceError::ManifestMismatch);
        }
        Ok(())
    }
}

fn validate_pages(pages: &[PreparedImageReceipt]) -> Result<(), ProvenanceError> {
    let mut previous = "";
    for page in pages {
        let [width, height] = page.dimensions;
        if page.name.as_str() <= previous
            || page.name.trim() != page.name
            || page.name.contains(['/', '\\', ':'])
            || page.name == "."
            || page.name.contains("..")
            || page.name.chars().any(char::is_control)
            || !digest_hex(&page.sha256)
            || width == 0
            || height == 0
            || page.mip_levels == 0
            || page.mip_levels > width.max(height).ilog2() + 1
            || page.safe_lod >= page.mip_levels
            || page.format.trim().is_empty()
            || page.format.chars().any(char::is_control)
        {
            return Err(ProvenanceError::Invalid(
                "malformed, unsorted, or duplicate prepared page",
            ));
        }
        previous = &page.name;
    }
    if pages.is_empty() {
        return Err(ProvenanceError::Invalid("no prepared pages"));
    }
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(output, "{byte:02x}").expect("writing to String is infallible");
    }
    output
}

fn lower_hex(s: &str) -> bool {
    s.bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn digest_hex(s: &str) -> bool {
    s.len() == 64 && lower_hex(s)
}

#[cfg(test)]
mod tests;
