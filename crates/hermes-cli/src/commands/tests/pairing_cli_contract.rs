#[test]
fn pairing_approval_revocation_and_pending_cleanup_persist() {
    let root = tempfile::tempdir().unwrap();
    let output = run_scenario_in(root.path(), "pairing-lifecycle", "");
    let secret =
        std::fs::read_to_string(root.path().join("home/.hermes/fixture-approval")).unwrap();
    assert!(
        !output.contains(&secret),
        "pairing printed the full fixture credential"
    );
    assert_output(
        &output,
        &[
            "Paired devices (2):",
            "Fixture desktop",
            "(unnamed)",
            "⏳ pending",
            "Device 'fixture-active' approved.",
            "Device 'fixture-active' is not pending (current status: approved)",
            "Cleared 1 pending pairing request(s).",
            "✓ approved",
            "Device 'fixture-active' revoked.",
            "Device 'fixture-pending' not found",
            "Paired devices (1):",
            "✗ revoked",
            "Failed to clear pending requests: Failed to parse",
        ],
    );
    let masked = output
        .lines()
        .find_map(|line| line.trim().strip_prefix("Shared secret: "))
        .expect("masked approval credential");
    assert_eq!(
        masked,
        format!("{}***{}", &secret[..4], &secret[secret.len() - 4..])
    );
}

async fn pairing_lifecycle(home: &Path) {
    use crate::pairing_store::{PairedDevice, PairingStatus, PairingStore};

    let path = PairingStore::default_path();
    assert_eq!(path, home.join("pairing.json"));
    let store = PairingStore::open_default();
    store
        .save(&[
            PairedDevice {
                device_id: "fixture-active".into(),
                name: Some("Fixture desktop".into()),
                status: PairingStatus::Pending,
                created_at: "2026-01-01T00:00:00Z".into(),
                last_seen: None,
                shared_secret: None,
            },
            PairedDevice {
                device_id: "fixture-pending".into(),
                name: None,
                status: PairingStatus::Pending,
                created_at: "2026-01-02T00:00:00Z".into(),
                last_seen: None,
                shared_secret: None,
            },
        ])
        .unwrap();
    handle_cli_pairing(None, None).await.unwrap();

    handle_cli_pairing(Some("approve".into()), Some("fixture-active".into()))
        .await
        .unwrap();
    let devices = store.list().unwrap();
    assert_eq!(devices[0].status, PairingStatus::Approved);
    let secret = devices[0].shared_secret.as_deref().unwrap();
    assert!(secret.len() == 64 && secret.is_ascii());
    std::fs::write(home.join("fixture-approval"), secret).unwrap();
    assert!(devices[0].last_seen.is_some());
    assert_eq!(devices[1].status, PairingStatus::Pending);
    handle_cli_pairing(Some("approve".into()), Some("fixture-active".into()))
        .await
        .unwrap();
    let repeated = store.list().unwrap();
    assert!(repeated[0].shared_secret == devices[0].shared_secret);
    assert_eq!(repeated[1].status, PairingStatus::Pending);

    handle_cli_pairing(Some("clear-pending".into()), None)
        .await
        .unwrap();
    let devices = store.list().unwrap();
    let [approved] = devices.as_slice() else {
        panic!("expected only the approved device");
    };
    assert_eq!(approved.device_id, "fixture-active");
    assert_eq!(approved.status, PairingStatus::Approved);
    assert!(approved.shared_secret.is_some());
    handle_cli_pairing(None, None).await.unwrap();

    for device in ["fixture-pending", "fixture-active"] {
        handle_cli_pairing(Some("revoke".into()), Some(device.into()))
            .await
            .unwrap();
    }
    let devices = store.list().unwrap();
    let [revoked] = devices.as_slice() else {
        panic!("expected only the revoked device");
    };
    assert_eq!(revoked.device_id, "fixture-active");
    assert_eq!(revoked.status, PairingStatus::Revoked);
    assert!(revoked.shared_secret.is_none());
    handle_cli_pairing(None, None).await.unwrap();

    std::fs::write(&path, "{").unwrap();
    assert_io_error(
        handle_cli_pairing(None, None).await.unwrap_err(),
        "Failed to parse",
    );
    handle_cli_pairing(Some("clear-pending".into()), None)
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), "{");
}
