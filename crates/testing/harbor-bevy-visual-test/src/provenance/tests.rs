use bevy::{
    app::Update,
    asset::Assets,
    ecs::schedule::{IntoScheduleConfigs, Schedule, ScheduleLabel, SystemSet},
    render::{
        RenderSystems,
        extract_plugin::ExtractPlugin,
        extract_resource::ExtractResourcePlugin,
        render_resource::{Extent3d, TextureUsages},
    },
};

use super::*;

fn receipt(image: Handle<Image>) -> ImageProvenanceReceipt {
    ImageProvenanceReceipt {
        name: "page.ktx2".into(),
        sha256: sha256(b"page"),
        handle: image,
        dimensions: [16, 8],
        mip_levels: 3,
        safe_lod: 2,
        format: TextureFormat::Rgba8UnormSrgb,
    }
}

#[test]
fn extraction_and_gpu_readiness_do_not_require_initializing_a_gpu() {
    let mut app = App::new();
    assert!(matches!(
        verify_prepared_gpu_images(&app),
        Err(ProvenanceError::MissingMainReceipts)
    ));
    app.init_resource::<Assets<Image>>();
    let handle = app
        .world_mut()
        .resource_mut::<Assets<Image>>()
        .add(Image::default());
    let original = VisualImageProvenance(vec![receipt(handle)]);
    app.insert_resource(original.clone());
    assert!(matches!(
        verify_prepared_gpu_images(&app),
        Err(ProvenanceError::MissingRenderApp)
    ));

    app.add_plugins((
        ExtractPlugin::<RenderApp>::new(
            |_, _| {},
            || {
                let mut schedule = Schedule::new(Update);
                schedule.configure_sets(
                    (RenderSystems::ExtractCommands, RenderSystems::PostCleanup).chain(),
                );
                schedule
            },
            Update.intern(),
            RenderSystems::ExtractCommands.intern(),
            RenderSystems::PostCleanup.intern(),
        ),
        ExtractResourcePlugin::<VisualImageProvenance>::default(),
    ));
    app.sub_app_mut(RenderApp).update_schedule = Some(Update.intern());
    assert!(matches!(
        verify_prepared_gpu_images(&app),
        Err(ProvenanceError::MissingExtractedReceipts)
    ));
    app.update_sub_app_by_label(RenderApp);
    assert_eq!(
        app.sub_app(RenderApp)
            .world()
            .resource::<VisualImageProvenance>(),
        &original
    );
    assert!(matches!(
        verify_prepared_gpu_images(&app),
        Err(ProvenanceError::MissingRenderImages)
    ));

    app.sub_app_mut(RenderApp)
        .world_mut()
        .init_resource::<RenderAssets<GpuImage>>();
    assert!(
        matches!(verify_prepared_gpu_images(&app), Err(ProvenanceError::ImageNotPrepared(name)) if name == "page.ktx2")
    );
    app.world_mut().resource_mut::<VisualImageProvenance>().0[0].safe_lod = 1;
    assert!(matches!(
        verify_prepared_gpu_images(&app),
        Err(ProvenanceError::StaleReceipts)
    ));
    app.update_sub_app_by_label(RenderApp);
    assert!(matches!(
        verify_prepared_gpu_images(&app),
        Err(ProvenanceError::ImageNotPrepared(_))
    ));
}

#[test]
fn prepared_descriptor_requires_exact_size_mips_format_and_uploaded_data() {
    let receipt = receipt(Handle::default());
    for (size, mip_levels, format, depth, had_data, expected) in [
        ([16, 8], 3, TextureFormat::Rgba8UnormSrgb, 1, true, true),
        ([16, 4], 3, TextureFormat::Rgba8UnormSrgb, 1, true, false),
        ([16, 8], 2, TextureFormat::Rgba8UnormSrgb, 1, true, false),
        ([16, 8], 3, TextureFormat::Rgba8Unorm, 1, true, false),
        ([16, 8], 3, TextureFormat::Rgba8UnormSrgb, 2, true, false),
        ([16, 8], 3, TextureFormat::Rgba8UnormSrgb, 1, false, false),
    ] {
        let descriptor = TextureDescriptor {
            label: None,
            size: Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: depth,
            },
            mip_level_count: mip_levels,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format,
            usage: TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        };
        assert_eq!(
            verify_descriptor(&receipt, &descriptor, had_data).is_ok(),
            expected
        );
    }
}

#[test]
fn manifest_is_canonical_and_detects_changes() {
    let adapter = GpuAdapterIdentity {
        name: "Example adapter".into(),
        backend: "Vulkan".into(),
        device_type: "DiscreteGpu".into(),
        ..Default::default()
    };
    let first = PreparedImageReceipt::from(&receipt(Handle::default()));
    let second = PreparedImageReceipt {
        name: "second.ktx2".into(),
        ..first.clone()
    };
    let head = "1".repeat(40);
    let lock = b"example lock";
    let pack = [7; 32];
    let manifest = CaptureManifest::new(
        &head,
        lock,
        pack,
        adapter.clone(),
        vec![second.clone(), first.clone()],
    )
    .unwrap();
    let other = CaptureManifest::new(&head, lock, pack, adapter, vec![first, second]).unwrap();
    let json = manifest.to_json().unwrap();
    assert_eq!(json, other.to_json().unwrap());
    let parsed = CaptureManifest::from_json(&json).unwrap();
    assert_eq!(parsed, manifest);
    assert_eq!(parsed.cargo_lock_sha256, sha256(lock));
    assert!(parsed.verify_against(&manifest).is_ok());
    let changes: [fn(&mut CaptureManifest); 5] = [
        |m| m.git_head = "2".repeat(40),
        |m| m.cargo_lock_sha256 = sha256(b"another lock"),
        |m| m.asset_pack_sha256 = hex(&[8; 32]),
        |m| m.adapter.driver = "another driver".into(),
        |m| m.prepared_pages[0].sha256 = sha256(b"changed"),
    ];
    for change in changes {
        let mut changed = parsed.clone();
        change(&mut changed);
        assert!(matches!(
            changed.verify_against(&manifest),
            Err(ProvenanceError::ManifestMismatch)
        ));
    }
    let mut changed = parsed;
    changed.prepared_pages.reverse();
    assert!(CaptureManifest::from_json(&serde_json::to_vec(&changed).unwrap()).is_err());
    changed.prepared_pages.reverse();
    changed.prepared_pages[0].sha256.make_ascii_uppercase();
    assert!(changed.validate().is_err());
}

#[test]
fn manifests_reject_invalid_inputs_before_serializing() {
    let page = PreparedImageReceipt::from(&receipt(Handle::default()));
    let head = "3".repeat(64);
    let adapter = GpuAdapterIdentity {
        name: "Example adapter".into(),
        backend: "Vulkan".into(),
        device_type: "Cpu".into(),
        ..Default::default()
    };
    let new = |head, pages| CaptureManifest::new(head, b"lock", [1; 32], adapter.clone(), pages);
    for invalid in [
        PreparedImageReceipt {
            name: "../page.ktx2".into(),
            ..page.clone()
        },
        PreparedImageReceipt {
            safe_lod: 3,
            ..page.clone()
        },
        PreparedImageReceipt {
            dimensions: [16, 0],
            ..page.clone()
        },
        PreparedImageReceipt {
            mip_levels: 6,
            ..page.clone()
        },
    ] {
        assert!(new(&head, vec![invalid]).is_err());
    }
    assert!(new(&head, vec![page.clone(), page.clone()]).is_err());
    assert!(new(&head, Vec::new()).is_err());
    assert!(new("branch", vec![page.clone()]).is_err());
    let mut valid = new(&head, vec![page]).unwrap();
    valid.schema_version += 1;
    assert!(valid.to_json().is_err());
}
