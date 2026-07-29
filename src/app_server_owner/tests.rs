use super::*;

#[test]
fn owner_record_round_trips_and_matches_exact_identity() {
    let root = tempfile::tempdir().unwrap();
    let binary = root.path().join("codex");
    fs::write(&binary, b"binary").unwrap();
    let socket = root.path().join("control.sock");
    let path = root.path().join("state/app-server-owner.json");
    let owner =
        AppServerOwner::new(42, &binary, "codex-cli test-version".to_owned(), &socket).unwrap();

    save(&path, &owner).unwrap();
    assert_eq!(load(&path).unwrap(), Some(owner.clone()));
    assert!(owner.matches(&binary, "codex-cli test-version", &socket));
    assert!(!owner.matches(&binary, "codex-cli other-version", &socket));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn removal_requires_the_recorded_pid() {
    let root = tempfile::tempdir().unwrap();
    let binary = root.path().join("codex");
    fs::write(&binary, b"binary").unwrap();
    let path = root.path().join("app-server-owner.json");
    let owner = AppServerOwner::new(
        42,
        &binary,
        "codex-cli test-version".to_owned(),
        &root.path().join("control.sock"),
    )
    .unwrap();
    save(&path, &owner).unwrap();

    remove_if_pid(&path, 7).unwrap();
    assert!(path.exists());
    remove_if_pid(&path, 42).unwrap();
    assert!(!path.exists());
}
