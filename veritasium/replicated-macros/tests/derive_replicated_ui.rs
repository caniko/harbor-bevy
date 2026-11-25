#[test]
fn derive_replicated_ui() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/derive_replicated_ui/garbage_attr.rs");
    t.compile_fail("tests/derive_replicated_ui/garbage_resource_attr.rs");
    t.compile_fail("tests/derive_replicated_ui/missing_component_id.rs");
    t.compile_fail("tests/derive_replicated_ui/missing_resource_id.rs");
}
