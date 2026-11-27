//! Pinned flat asset packs and bounded texture validation.

mod ktx2;
mod pack;

pub use ktx2::{
    Ktx2HeaderInfo, Ktx2LevelInfo, Ktx2PixelFormat, Ktx2Supercompression, Ktx2ValidationError,
};
pub use pack::{
    AssetPackError, AssetPackPolicy, VerifiedAssetPack, load_and_verify_dir,
    load_and_verify_embedded,
};

#[cfg(test)]
mod tests;
