use std::fs;

#[cfg(unix)]
use std::{env, path::PathBuf, process::Command};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

use planeradarctl::{
    state::{
        ArtifactIdentity, InstallPhase, InstallState, LocalStateStore, OwnedFile,
        STATE_SCHEMA_VERSION, StateError, StateStore, TARGET_STATE_FILE_MODE, TARGET_STATE_OWNER,
        TARGET_STATE_PATH, TargetHardwareIdentity, TargetInstallState, TargetStateStoreContract,
    },
    target::{SshTarget, TargetIdentity},
};

fn identity() -> TargetIdentity {
    TargetIdentity {
        host_key_sha256: "SHA256:8R2K6pFDwIKY2fWb/4mMxwAA7PY8VYyLmWucTx7D99A".into(),
        model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
        serial: "10000000abcdef01".into(),
    }
}

fn artifact(version: &str, marker: char) -> ArtifactIdentity {
    ArtifactIdentity {
        version: version.into(),
        source_commit: marker.to_string().repeat(40),
        sha256: marker.to_string().repeat(64),
    }
}

fn state(phase: InstallPhase) -> InstallState {
    InstallState {
        schema_version: STATE_SCHEMA_VERSION,
        target: identity(),
        phase,
        application: (phase >= InstallPhase::ApplicationAcquired).then(|| artifact("0.1.0", 'a')),
        driver: (phase >= InstallPhase::DriverReady).then(|| artifact("0.1.0-rc.4", 'b')),
    }
}

fn store(home: &std::path::Path, expected: TargetIdentity) -> LocalStateStore {
    LocalStateStore::new(home, None, expected).expect("create local state store")
}

#[test]
fn state_round_trip_preserves_every_install_phase() {
    let temporary_directory = tempfile::tempdir().expect("temporary directory");
    let state_home = temporary_directory.path().join("home");
    let store = store(&state_home, identity());

    for phase in InstallPhase::ALL {
        let expected = state(phase);
        store.save(&expected).expect("save state");
        assert_eq!(store.load().expect("load state"), Some(expected));
    }
}

#[test]
fn state_json_rejects_unknown_schemas_unknown_fields_truncation_and_trailing_data() {
    let temporary_directory = tempfile::tempdir().expect("temporary directory");
    let store = store(&temporary_directory.path().join("home"), identity());
    fs::create_dir_all(store.state_path().parent().expect("state parent"))
        .expect("create state parent");

    let cases = [
        (
            "unknown schema",
            r#"{"schema_version":2,"target":{"host_key_sha256":"SHA256:8R2K6pFDwIKY2fWb/4mMxwAA7PY8VYyLmWucTx7D99A","model":"Raspberry Pi Zero 2 W Rev 1.0","serial":"10000000abcdef01"},"phase":"discovered","application":null,"driver":null}"#,
        ),
        (
            "unknown field",
            r#"{"schema_version":1,"target":{"host_key_sha256":"SHA256:8R2K6pFDwIKY2fWb/4mMxwAA7PY8VYyLmWucTx7D99A","model":"Raspberry Pi Zero 2 W Rev 1.0","serial":"10000000abcdef01","token":"not allowed"},"phase":"discovered","application":null,"driver":null}"#,
        ),
        ("truncated json", r#"{"schema_version":1,"#),
        (
            "trailing json",
            r#"{"schema_version":1,"target":{"host_key_sha256":"SHA256:8R2K6pFDwIKY2fWb/4mMxwAA7PY8VYyLmWucTx7D99A","model":"Raspberry Pi Zero 2 W Rev 1.0","serial":"10000000abcdef01"},"phase":"discovered","application":null,"driver":null}{}"#,
        ),
    ];

    for (name, contents) in cases {
        fs::write(store.state_path(), contents).expect("write malformed state");
        assert!(store.load().is_err(), "{name} must be rejected");
    }
}

#[test]
fn store_fails_closed_when_any_target_identity_component_differs() {
    let temporary_directory = tempfile::tempdir().expect("temporary directory");
    let state_home = temporary_directory.path().join("home");
    let original = identity();
    let first_store = store(&state_home, original.clone());
    first_store
        .save(&state(InstallPhase::PreflightPassed))
        .expect("save original state");

    for (name, expected) in [
        (
            "host key",
            TargetIdentity {
                host_key_sha256: "SHA256:other".into(),
                ..original.clone()
            },
        ),
        (
            "model",
            TargetIdentity {
                model: "Raspberry Pi 5".into(),
                ..original.clone()
            },
        ),
        (
            "serial",
            TargetIdentity {
                serial: "10000000abcdef02".into(),
                ..original.clone()
            },
        ),
    ] {
        assert_ne!(original, expected, "fixture must differ by {name}");
        assert!(
            !original.matches(&expected),
            "comparison must reject {name}"
        );

        if name == "host key" {
            let changed_store = store(&state_home, expected);
            assert_eq!(changed_store.load().expect("other target state"), None);
        } else {
            let changed_store = store(&state_home, expected);
            assert!(changed_store.load().is_err(), "{name} must fail closed");
        }
    }
}

#[test]
fn store_reports_an_identity_mismatch_for_each_valid_tampered_identity_field() {
    let temporary_directory = tempfile::tempdir().expect("temporary directory");
    let store = store(&temporary_directory.path().join("home"), identity());
    fs::create_dir_all(store.state_path().parent().expect("state parent"))
        .expect("create state parent");

    for (name, target) in [
        (
            "host key",
            TargetIdentity {
                host_key_sha256: "SHA256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
                ..identity()
            },
        ),
        (
            "model",
            TargetIdentity {
                model: "Raspberry Pi Zero 2 W Rev 1.1".into(),
                ..identity()
            },
        ),
        (
            "serial",
            TargetIdentity {
                serial: "10000000abcdef02".into(),
                ..identity()
            },
        ),
    ] {
        let tampered = InstallState {
            target,
            ..state(InstallPhase::Complete)
        };
        fs::write(
            store.state_path(),
            tampered.to_json().expect("valid tampered state"),
        )
        .expect("write same-path tampered state");
        #[cfg(unix)]
        fs::set_permissions(store.state_path(), fs::Permissions::from_mode(0o600))
            .expect("private tampered state mode");

        assert!(
            matches!(store.load(), Err(StateError::TargetIdentityMismatch)),
            "{name} must report an identity mismatch"
        );
    }
}

#[test]
fn fingerprint_key_is_deterministic_and_isolates_each_target() {
    let temporary_directory = tempfile::tempdir().expect("temporary directory");
    let state_home = temporary_directory.path().join("home");
    let first_identity = identity();
    let second_identity = TargetIdentity {
        host_key_sha256: "SHA256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        ..first_identity.clone()
    };
    let first = store(&state_home, first_identity.clone());
    let same_first = store(&state_home, first_identity);
    let second = store(&state_home, second_identity);

    assert_eq!(first.target_key(), same_first.target_key());
    assert_eq!(first.target_key().len(), 64);
    assert!(
        first
            .target_key()
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    );
    assert_ne!(first.state_path(), second.state_path());
    assert!(!first.state_path().to_string_lossy().contains("SHA256:"));

    first
        .save(&state(InstallPhase::ApplicationAcquired))
        .expect("save first target");
    second
        .save(&InstallState {
            target: TargetIdentity {
                host_key_sha256: "SHA256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                ..identity()
            },
            ..state(InstallPhase::DriverReady)
        })
        .expect("save second target");

    assert_eq!(
        first
            .load()
            .expect("load first")
            .expect("first state")
            .phase,
        InstallPhase::ApplicationAcquired
    );
    assert_eq!(
        second
            .load()
            .expect("load second")
            .expect("second state")
            .phase,
        InstallPhase::DriverReady
    );
}

#[cfg(unix)]
#[test]
fn save_uses_private_mode_and_safely_corrects_an_insecure_existing_mode() {
    let temporary_directory = tempfile::tempdir().expect("temporary directory");
    let store = store(&temporary_directory.path().join("home"), identity());
    store
        .save(&state(InstallPhase::Discovered))
        .expect("initial save");
    assert_eq!(
        fs::metadata(store.state_path()).expect("metadata").mode() & 0o777,
        0o600
    );

    fs::set_permissions(store.state_path(), fs::Permissions::from_mode(0o644))
        .expect("make mode insecure");
    assert!(
        store.load().is_err(),
        "load must reject an insecure state file"
    );

    store
        .save(&state(InstallPhase::Complete))
        .expect("safe atomic correction");
    assert_eq!(
        fs::metadata(store.state_path()).expect("metadata").mode() & 0o777,
        0o600
    );
    assert_eq!(
        store
            .load()
            .expect("load corrected state")
            .expect("state")
            .phase,
        InstallPhase::Complete
    );
}

#[test]
fn failed_save_preserves_the_prior_valid_state() {
    let temporary_directory = tempfile::tempdir().expect("temporary directory");
    let store = store(&temporary_directory.path().join("home"), identity());
    let original = state(InstallPhase::PreflightPassed);
    store.save(&original).expect("save original state");

    let invalid = InstallState {
        schema_version: STATE_SCHEMA_VERSION + 1,
        ..state(InstallPhase::Complete)
    };
    assert!(
        store.save(&invalid).is_err(),
        "invalid state must not publish"
    );

    assert_eq!(store.load().expect("load prior state"), Some(original));
}

#[cfg(unix)]
#[test]
fn atomic_replacement_leaves_a_hard_link_to_the_prior_valid_state_unchanged() {
    let temporary_directory = tempfile::tempdir().expect("temporary directory");
    let store = store(&temporary_directory.path().join("home"), identity());
    let original = state(InstallPhase::Discovered);
    store.save(&original).expect("save original state");
    let retained_prior = store.state_path().with_file_name("retained-prior.json");
    fs::hard_link(store.state_path(), &retained_prior).expect("retain prior inode");

    store
        .save(&state(InstallPhase::Complete))
        .expect("atomically replace state");

    assert_eq!(
        InstallState::from_json(
            &fs::read_to_string(&retained_prior).expect("read retained prior state"),
        )
        .expect("parse retained prior state"),
        original
    );
    assert_eq!(
        store
            .load()
            .expect("load replacement")
            .expect("state")
            .phase,
        InstallPhase::Complete
    );
}

#[cfg(unix)]
#[test]
fn store_refuses_symlink_and_non_regular_final_paths_without_following_them() {
    let temporary_directory = tempfile::tempdir().expect("temporary directory");
    let state_home = temporary_directory.path().join("home");
    let store = store(&state_home, identity());
    let parent = store.state_path().parent().expect("state parent");
    fs::create_dir_all(parent).expect("create state parent");

    let outside = temporary_directory.path().join("outside-state.json");
    fs::write(&outside, b"outside state").expect("write outside state");
    symlink(&outside, store.state_path()).expect("create state symlink");
    assert!(store.load().is_err(), "load must refuse symlink");
    assert!(
        store.save(&state(InstallPhase::Discovered)).is_err(),
        "save must refuse symlink"
    );
    assert_eq!(
        fs::read(&outside).expect("read outside state"),
        b"outside state"
    );

    fs::remove_file(store.state_path()).expect("remove symlink");
    fs::create_dir(store.state_path()).expect("create final directory");
    fs::write(store.state_path().join("keep"), b"do not replace")
        .expect("write destination fixture");
    assert!(store.load().is_err(), "load must refuse directory");
    assert!(
        store.save(&state(InstallPhase::Discovered)).is_err(),
        "save must refuse directory"
    );
    assert_eq!(
        fs::read(store.state_path().join("keep")).expect("read destination fixture"),
        b"do not replace"
    );
}

#[cfg(unix)]
#[test]
fn store_refuses_a_symlinked_fingerprint_parent_without_leaking_outside_state() {
    let temporary_directory = tempfile::tempdir().expect("temporary directory");
    let home = temporary_directory.path().join("home");
    let store = store(&home, identity());
    let state_root = LocalStateStore::resolve_state_root(&home, None).expect("state root");
    let fingerprint_parent = store.state_path().parent().expect("fingerprint parent");
    fs::create_dir_all(state_root.join("planeradar/installer")).expect("create installer parent");

    let outside = temporary_directory.path().join("outside");
    fs::create_dir(&outside).expect("create outside directory");
    let outside_state = outside.join("state.json");
    fs::write(
        &outside_state,
        state(InstallPhase::Complete)
            .to_json()
            .expect("serialize outside state"),
    )
    .expect("write outside state");
    fs::set_permissions(&outside_state, fs::Permissions::from_mode(0o600))
        .expect("private outside state");
    let sentinel = outside.join("keep");
    fs::write(&sentinel, b"do not touch").expect("write outside sentinel");
    let before_state = fs::read(&outside_state).expect("read outside state");
    let before_sentinel = fs::read(&sentinel).expect("read outside sentinel");
    symlink(&outside, fingerprint_parent).expect("symlink fingerprint parent");

    assert!(matches!(
        store.load(),
        Err(StateError::UnsafeParentPath { .. })
    ));
    assert!(matches!(
        store.save(&state(InstallPhase::Discovered)),
        Err(StateError::UnsafeParentPath { .. })
    ));
    assert_eq!(
        fs::read(&outside_state).expect("read outside state"),
        before_state
    );
    assert_eq!(
        fs::read(&sentinel).expect("read outside sentinel"),
        before_sentinel
    );
    let mut outside_entries = fs::read_dir(&outside)
        .expect("read outside directory")
        .map(|entry| entry.expect("outside entry").file_name())
        .collect::<Vec<_>>();
    outside_entries.sort();
    assert_eq!(
        outside_entries,
        vec![
            std::ffi::OsString::from("keep"),
            std::ffi::OsString::from("state.json")
        ]
    );
}

#[cfg(unix)]
#[test]
fn store_refuses_a_symlinked_installer_parent_component() {
    let temporary_directory = tempfile::tempdir().expect("temporary directory");
    let home = temporary_directory.path().join("home");
    let store = store(&home, identity());
    let state_root = LocalStateStore::resolve_state_root(&home, None).expect("state root");
    let installer_parent = state_root.join("planeradar/installer");
    fs::create_dir_all(state_root.join("planeradar")).expect("create planeradar parent");

    let outside = temporary_directory.path().join("outside");
    fs::create_dir(&outside).expect("create outside directory");
    let sentinel = outside.join("keep");
    fs::write(&sentinel, b"do not touch").expect("write outside sentinel");
    symlink(&outside, installer_parent).expect("symlink installer parent");

    assert!(matches!(
        store.load(),
        Err(StateError::UnsafeParentPath { .. })
    ));
    assert!(matches!(
        store.save(&state(InstallPhase::Discovered)),
        Err(StateError::UnsafeParentPath { .. })
    ));
    assert_eq!(
        fs::read(&sentinel).expect("read outside sentinel"),
        b"do not touch"
    );
    let outside_entries = fs::read_dir(&outside)
        .expect("read outside directory")
        .map(|entry| entry.expect("outside entry").file_name())
        .collect::<Vec<_>>();
    assert_eq!(outside_entries, vec![std::ffi::OsString::from("keep")]);
}

#[cfg(unix)]
#[test]
fn owned_state_directories_request_private_mode_at_creation() {
    let source = include_str!("../src/state.rs");
    let start = source
        .find("fn create_owned_state_directory_safely")
        .expect("directory creation helper");
    let after_start = &source[start..];
    let function = &after_start[..after_start
        .find("\nfn ")
        .expect("next helper after directory creation")];

    assert!(
        !function.contains("fs::create_dir(path)"),
        "a chmod after create_dir leaves a permissive-umask window"
    );
    assert!(
        function.contains("create_directory_with_private_mode(path)"),
        "Unix directories must be created with the private mode requested"
    );
    let helper_start = source
        .find("fn create_directory_with_private_mode")
        .expect("private-mode creation helper");
    let helper_after_start = &source[helper_start..];
    let helper = &helper_after_start[..helper_after_start
        .find("\n#[cfg(not(unix))]")
        .expect("non-Unix creation fallback")];
    assert!(
        helper.contains("builder.mode(0o700)"),
        "the Unix creation syscall must request 0700, not chmod afterwards"
    );
}

#[cfg(unix)]
#[test]
fn store_creates_private_owned_directories_when_child_umask_is_zero() {
    let temporary_directory = tempfile::tempdir().expect("temporary directory");
    let home = temporary_directory.path().join("home");
    let status = Command::new(env::current_exe().expect("current test executable"))
        .args([
            "--exact",
            "store_creates_private_owned_directories_when_child_umask_is_zero_helper",
            "--ignored",
        ])
        .env("PLANERADARCTL_TEST_STATE_HOME", &home)
        .status()
        .expect("run isolated umask helper");
    assert!(status.success(), "umask helper must pass");

    let store = store(&home, identity());
    let state_root = LocalStateStore::resolve_state_root(&home, None).expect("state root");
    for directory in [
        state_root.join("planeradar"),
        state_root.join("planeradar/installer"),
        store
            .state_path()
            .parent()
            .expect("fingerprint parent")
            .to_owned(),
    ] {
        assert_eq!(
            fs::metadata(&directory)
                .expect("created directory")
                .permissions()
                .mode()
                & 0o777,
            0o700,
            "{} must be private even with umask 000",
            directory.display()
        );
    }
}

#[cfg(unix)]
#[test]
#[ignore = "only run in the isolated umask-zero subprocess"]
fn store_creates_private_owned_directories_when_child_umask_is_zero_helper() {
    let home = PathBuf::from(
        env::var_os("PLANERADARCTL_TEST_STATE_HOME").expect("helper state home environment"),
    );
    unsafe {
        libc::umask(0);
    }
    store(&home, identity())
        .save(&state(InstallPhase::Discovered))
        .expect("save with umask zero");
}

#[test]
fn persisted_schemas_have_no_credential_or_local_machine_fields() {
    let local_json = state(InstallPhase::Complete)
        .to_json()
        .expect("serialize local state");
    let target_json = target_state().to_json().expect("serialize target state");

    for json in [local_json, target_json] {
        for forbidden in [
            "password",
            "credential",
            "token",
            "secret",
            "location",
            "mac_path",
            "/Users/",
        ] {
            assert!(!json.contains(forbidden), "state contains {forbidden}");
        }
    }
}

#[test]
fn persisted_identity_fields_reject_mac_paths_settings_and_secret_like_values() {
    let mut mac_path = state(InstallPhase::Discovered);
    mac_path.target.model = "/Users/shayne/private-model".into();
    assert!(mac_path.to_json().is_err(), "Mac path must be rejected");

    let mut credential = state(InstallPhase::Discovered);
    credential.target.host_key_sha256 = "SHA256:token=private".into();
    assert!(
        credential.to_json().is_err(),
        "credential-like host key must be rejected"
    );

    let mut location = target_state();
    location.hardware.serial = "location=40.7128,-74.0060".into();
    assert!(
        location.to_json().is_err(),
        "location-like serial must be rejected"
    );
}

fn target_state() -> TargetInstallState {
    TargetInstallState {
        schema_version: STATE_SCHEMA_VERSION,
        hardware: TargetHardwareIdentity {
            model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
            serial: "10000000abcdef01".into(),
        },
        application: Some(artifact("0.1.0", 'a')),
        driver: Some(artifact("0.1.0-rc.4", 'b')),
        owned_files: vec![OwnedFile {
            target_path: "/usr/local/lib/planeradar/planeradar".into(),
            sha256: "c".repeat(64),
        }],
        last_verified_phase: InstallPhase::FinalVerified,
    }
}

#[test]
fn target_state_contract_round_trips_strictly_at_the_fixed_target_path() {
    let original = target_state();
    let encoded = original.to_json().expect("serialize target state");

    assert_eq!(
        TARGET_STATE_PATH,
        "/var/lib/planeradar-installer/state.json"
    );
    assert_eq!(
        TargetInstallState::from_json(&encoded).expect("parse target state"),
        original
    );
    assert!(TargetInstallState::from_json(&format!("{encoded} trailing")).is_err());
    assert!(
        TargetInstallState::from_json(&encoded.replace(
            "\"last_verified_phase\"",
            "\"unexpected\":true,\"last_verified_phase\"",
        ))
        .is_err()
    );
}

#[test]
fn state_rejects_artifacts_that_do_not_match_the_persisted_phase() {
    let discovered_with_application = serde_json::json!({
        "schema_version": 1,
        "target": identity(),
        "phase": "discovered",
        "application": artifact("0.1.0", 'a'),
        "driver": null
    });
    let acquired_without_application = serde_json::json!({
        "schema_version": 1,
        "target": identity(),
        "phase": "application_acquired",
        "application": null,
        "driver": null
    });
    let driver_ready_without_driver = serde_json::json!({
        "schema_version": 1,
        "target": identity(),
        "phase": "driver_ready",
        "application": artifact("0.1.0", 'a'),
        "driver": null
    });

    for invalid in [
        discovered_with_application,
        acquired_without_application,
        driver_ready_without_driver,
    ] {
        assert!(InstallState::from_json(&invalid.to_string()).is_err());
    }
}

#[test]
fn target_state_requires_exact_artifacts_and_owned_files_for_its_phase() {
    let mut missing_application = target_state();
    missing_application.last_verified_phase = InstallPhase::Complete;
    missing_application.application = None;

    let mut missing_driver = target_state();
    missing_driver.last_verified_phase = InstallPhase::DriverReady;
    missing_driver.driver = None;
    missing_driver.owned_files.clear();

    let mut missing_owned_files = target_state();
    missing_owned_files.last_verified_phase = InstallPhase::ApplicationInstalled;
    missing_owned_files.owned_files.clear();

    let mut premature_owned_files = target_state();
    premature_owned_files.last_verified_phase = InstallPhase::DriverAccepted;

    for invalid in [
        missing_application,
        missing_driver,
        missing_owned_files,
        premature_owned_files,
    ] {
        assert!(invalid.to_json().is_err());
    }
}

#[test]
fn target_state_store_contract_requires_root_private_atomic_durable_writes() {
    let contract = TargetStateStoreContract::required();

    assert_eq!(contract.path, TARGET_STATE_PATH);
    assert_eq!(contract.owner, TARGET_STATE_OWNER);
    assert_eq!(contract.file_mode, TARGET_STATE_FILE_MODE);
    assert!(contract.serializes_before_publish);
    assert!(contract.uses_same_directory_temporary_file);
    assert!(contract.syncs_file_before_publish);
    assert!(contract.atomically_replaces_final_path);
    assert!(contract.syncs_parent_directory_after_publish);
    assert!(contract.refuses_unsafe_final_path);
}

#[test]
fn state_root_uses_absolute_xdg_override_or_an_absolute_home_default() {
    let temporary_directory = tempfile::tempdir().expect("temporary directory");
    let home = temporary_directory.path().join("home");
    let xdg_state_home = temporary_directory.path().join("xdg-state");
    let xdg_store = LocalStateStore::new(&home, Some(&xdg_state_home), identity())
        .expect("absolute XDG state home");
    let default_store = LocalStateStore::new(&home, None, identity()).expect("absolute home");

    assert!(xdg_store.state_path().starts_with(&xdg_state_home));
    assert!(
        default_store
            .state_path()
            .starts_with(home.join(".local/state"))
    );
    assert!(LocalStateStore::new(std::path::Path::new("relative-home"), None, identity()).is_err());
    assert!(
        LocalStateStore::new(
            &home,
            Some(std::path::Path::new("relative-xdg-state")),
            identity(),
        )
        .is_err()
    );
}

#[test]
fn ssh_target_accepts_only_user_at_hostname_or_ipv4() {
    for (input, expected_user, expected_host) in [
        ("alice@radar.local", "alice", "radar.local"),
        ("ops_2@hangar-2", "ops_2", "hangar-2"),
        ("operator@192.0.2.44", "operator", "192.0.2.44"),
    ] {
        let target: SshTarget = input.parse().expect(input);
        assert_eq!(target.username().as_str(), expected_user);
        assert_eq!(target.host().as_str(), expected_host);
        assert_eq!(target.ssh_arguments(), ["--", input]);
    }
}

#[test]
fn ssh_target_rejects_every_hostile_input_class() {
    for input in [
        "root@radar.local",
        "alice @radar.local",
        "alice@radar.local\n",
        "ssh://alice@radar.local",
        "alice@radar.local:22",
        "alice@2001:db8::1",
        "alice@x@y",
        "@radar.local",
        "alice@",
        "-alice@radar.local",
        "alice@-radar.local",
        "alice@radar;touch",
        "alice@$(hostname)",
        "alice@bad_.local",
        "alice@radar..local",
        "alice@radar-.local",
        "alice@999.1.1.1",
        "alice@01.2.3.4",
        "alice@1.2.3",
    ] {
        assert!(
            input.parse::<SshTarget>().is_err(),
            "{input} must be rejected"
        );
    }
}

#[test]
fn ssh_destination_is_an_argument_vector_not_a_shell_command() {
    let target: SshTarget = "alice@radar.local".parse().expect("valid target");

    assert_eq!(target.ssh_arguments(), ["--", "alice@radar.local"]);
    assert_eq!(target.ssh_destination(), "alice@radar.local");
}
