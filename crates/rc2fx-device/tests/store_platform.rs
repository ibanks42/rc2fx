#[cfg(not(unix))]
#[test]
fn unsupported_platform_never_claims_durable_storage() {
    use rc2fx_device::store::{Store, StoreError};
    let temporary = tempfile::tempdir().unwrap();
    assert!(
        matches!(Store::open(temporary.path(), rc2fx_core::Config::default()), Err(StoreError::Io(error)) if error.kind() == std::io::ErrorKind::Unsupported)
    );
    assert_eq!(std::fs::read_dir(temporary.path()).unwrap().count(), 0);
}
