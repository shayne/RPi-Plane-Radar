use std::cell::{Cell, RefCell};
use std::process::Command;

use clap::Parser;
use planeradarctl::{
    cli::{Cli, Command as CliCommand},
    operations::{
        AcceptedPair, LifecycleBackend, LifecycleError, LifecycleManager, LifecycleOutcome,
        LifecycleState, ReleasePair,
    },
    state::{ArtifactIdentity, OwnedFile, TargetHardwareIdentity, TargetInstallState},
};
use semver::Version;

const APP_PATH: &str = "/opt/planeradar/bin/planeradar";

fn artifact(version: &str, seed: char) -> ArtifactIdentity {
    ArtifactIdentity {
        version: version.into(),
        source_commit: seed.to_string().repeat(40),
        sha256: seed.to_string().repeat(64),
    }
}

fn pair(version: &str, app_seed: char, driver_seed: char) -> ReleasePair {
    ReleasePair {
        application: artifact(version, app_seed),
        driver: artifact("0.1.0", driver_seed),
    }
}

fn accepted(version: &str, app_seed: char, driver_seed: char, sequence: u64) -> AcceptedPair {
    AcceptedPair {
        pair: pair(version, app_seed, driver_seed),
        sequence,
        owned_files: vec![OwnedFile {
            target_path: APP_PATH.into(),
            sha256: app_seed.to_string().repeat(64),
        }],
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Call {
    Resolve,
    StageApplication,
    StageDriver,
    TrybootDriver,
    VerifyTryboot,
    CommitDriver,
    RebootNormal,
    ActivateApplication,
    RestartApplication,
    VerifyPair,
    RestoreApplication,
    RestoreDriver,
    UninstallApplication,
    UninstallDriver,
    FinalizeUninstall,
}

struct FakeBackend {
    state: RefCell<LifecycleState>,
    release: RefCell<ReleasePair>,
    owned: RefCell<Vec<OwnedFile>>,
    calls: RefCell<Vec<Call>>,
    fail: Cell<Option<Call>>,
    requested: RefCell<Vec<Option<Version>>>,
}

impl FakeBackend {
    fn installed(history: Vec<AcceptedPair>, release: ReleasePair) -> Self {
        Self {
            state: RefCell::new(
                LifecycleState::installed(
                    TargetHardwareIdentity {
                        model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
                        serial: "10000000abcdef01".into(),
                    },
                    history,
                )
                .expect("valid lifecycle state"),
            ),
            release: RefCell::new(release),
            owned: RefCell::new(vec![OwnedFile {
                target_path: APP_PATH.into(),
                sha256: "f".repeat(64),
            }]),
            calls: RefCell::new(Vec::new()),
            fail: Cell::new(None),
            requested: RefCell::new(Vec::new()),
        }
    }

    fn call(&self, call: Call) -> Result<(), LifecycleError> {
        self.calls.borrow_mut().push(call);
        if self.fail.get() == Some(call) {
            Err(LifecycleError::Backend)
        } else {
            Ok(())
        }
    }
}

impl LifecycleBackend for FakeBackend {
    fn load_lifecycle_state(&self) -> Result<LifecycleState, LifecycleError> {
        Ok(self.state.borrow().clone())
    }

    fn save_lifecycle_state(&self, state: &LifecycleState) -> Result<(), LifecycleError> {
        *self.state.borrow_mut() = state.clone();
        Ok(())
    }

    fn resolve_release(&self, requested: Option<&Version>) -> Result<ReleasePair, LifecycleError> {
        self.call(Call::Resolve)?;
        self.requested.borrow_mut().push(requested.cloned());
        Ok(self.release.borrow().clone())
    }

    fn stage_application(&self, _pair: &ReleasePair) -> Result<(), LifecycleError> {
        self.call(Call::StageApplication)
    }

    fn stage_driver(&self, _pair: &ReleasePair) -> Result<(), LifecycleError> {
        self.call(Call::StageDriver)
    }

    fn tryboot_driver(&self, _pair: &ReleasePair) -> Result<(), LifecycleError> {
        self.call(Call::TrybootDriver)
    }

    fn verify_tryboot_driver(&self, _pair: &ReleasePair) -> Result<(), LifecycleError> {
        self.call(Call::VerifyTryboot)
    }

    fn commit_driver(&self, _pair: &ReleasePair) -> Result<(), LifecycleError> {
        self.call(Call::CommitDriver)
    }

    fn reboot_normal(&self, _pair: &ReleasePair) -> Result<(), LifecycleError> {
        self.call(Call::RebootNormal)
    }

    fn activate_application(&self, _pair: &ReleasePair) -> Result<Vec<OwnedFile>, LifecycleError> {
        self.call(Call::ActivateApplication)?;
        Ok(self.owned.borrow().clone())
    }

    fn restart_application(&self) -> Result<(), LifecycleError> {
        self.call(Call::RestartApplication)
    }

    fn verify_pair(&self, _pair: &ReleasePair) -> Result<(), LifecycleError> {
        self.call(Call::VerifyPair)
    }

    fn restore_application(&self, _prior: &AcceptedPair) -> Result<(), LifecycleError> {
        self.call(Call::RestoreApplication)
    }

    fn restore_driver(&self, _prior: &AcceptedPair) -> Result<(), LifecycleError> {
        self.call(Call::RestoreDriver)
    }

    fn uninstall_application(
        &self,
        _owned_files: &[OwnedFile],
        _purge_settings: bool,
    ) -> Result<(), LifecycleError> {
        self.call(Call::UninstallApplication)
    }

    fn uninstall_driver(&self, _driver: &ArtifactIdentity) -> Result<(), LifecycleError> {
        self.call(Call::UninstallDriver)
    }

    fn finalize_uninstall(&self, state: &LifecycleState) -> Result<(), LifecycleError> {
        self.call(Call::FinalizeUninstall)?;
        *self.state.borrow_mut() = LifecycleState::empty(state.hardware().clone())?;
        Ok(())
    }
}

#[test]
fn application_only_upgrade_is_atomic_healthy_and_never_touches_driver_or_reboot() {
    let current = accepted("1.0.0", '1', 'a', 7);
    let backend = FakeBackend::installed(vec![current], pair("1.1.0", '2', 'a'));

    let outcome = LifecycleManager::new(&backend)
        .upgrade(Some(&Version::parse("1.1.0").unwrap()))
        .expect("application-only upgrade");

    assert_eq!(
        outcome,
        LifecycleOutcome::Accepted {
            version: Version::parse("1.1.0").unwrap(),
            driver_changed: false,
        }
    );
    assert_eq!(
        backend.calls.borrow().as_slice(),
        [
            Call::Resolve,
            Call::StageApplication,
            Call::ActivateApplication,
            Call::RestartApplication,
            Call::VerifyPair,
        ]
    );
    assert_eq!(
        backend.requested.borrow().as_slice(),
        [Some(Version::parse("1.1.0").unwrap())]
    );
}

#[test]
fn accepted_history_keeps_current_and_only_two_prior_pairs_in_deterministic_order() {
    let backend = FakeBackend::installed(
        vec![
            accepted("1.2.0", '3', 'a', 12),
            accepted("1.1.0", '2', 'a', 11),
            accepted("1.0.0", '1', 'a', 10),
        ],
        pair("1.3.0", '4', 'a'),
    );

    LifecycleManager::new(&backend)
        .upgrade(None)
        .expect("upgrade");

    let state = backend.state.borrow();
    assert_eq!(
        state
            .accepted()
            .iter()
            .map(|accepted| accepted.pair.application.version.as_str())
            .collect::<Vec<_>>(),
        ["1.3.0", "1.2.0", "1.1.0"]
    );
    assert_eq!(
        state
            .accepted()
            .iter()
            .map(|accepted| accepted.sequence)
            .collect::<Vec<_>>(),
        [13, 12, 11]
    );
}

#[test]
fn every_application_failure_restores_and_proves_the_prior_pair() {
    for failure in [
        Call::ActivateApplication,
        Call::RestartApplication,
        Call::VerifyPair,
    ] {
        let current = accepted("1.0.0", '1', 'a', 7);
        let backend = FakeBackend::installed(vec![current.clone()], pair("1.1.0", '2', 'a'));
        backend.fail.set(Some(failure));

        assert!(LifecycleManager::new(&backend).upgrade(None).is_err());
        assert_eq!(
            backend
                .calls
                .borrow()
                .iter()
                .rev()
                .take(3)
                .copied()
                .collect::<Vec<_>>(),
            [
                Call::VerifyPair,
                Call::RestartApplication,
                Call::RestoreApplication
            ],
            "{failure:?}"
        );
        assert_eq!(backend.state.borrow().accepted(), &[current]);
    }
}

#[test]
fn driver_change_uses_exact_tryboot_commit_normal_boot_sequence() {
    let current = accepted("1.0.0", '1', 'a', 7);
    let backend = FakeBackend::installed(vec![current], pair("2.0.0", '2', 'b'));

    LifecycleManager::new(&backend)
        .upgrade(None)
        .expect("driver-changing upgrade");

    assert_eq!(
        backend.calls.borrow().as_slice(),
        [
            Call::Resolve,
            Call::StageApplication,
            Call::StageDriver,
            Call::TrybootDriver,
            Call::VerifyTryboot,
            Call::CommitDriver,
            Call::RebootNormal,
            Call::ActivateApplication,
            Call::RestartApplication,
            Call::VerifyPair,
        ]
    );
}

#[test]
fn driver_change_failure_restores_the_last_exact_application_and_driver_pair() {
    let current = accepted("1.0.0", '1', 'a', 7);
    let backend = FakeBackend::installed(vec![current.clone()], pair("2.0.0", '2', 'b'));
    backend.fail.set(Some(Call::VerifyTryboot));

    assert!(LifecycleManager::new(&backend).upgrade(None).is_err());

    let calls = backend.calls.borrow();
    assert!(calls.ends_with(&[
        Call::RestoreDriver,
        Call::RestoreApplication,
        Call::RestartApplication,
        Call::VerifyPair,
    ]));
    assert_eq!(backend.state.borrow().accepted(), &[current]);
}

#[test]
fn rollback_chooses_newest_prior_or_an_explicit_accepted_version() {
    let history = vec![
        accepted("1.2.0", '3', 'a', 12),
        accepted("1.1.0", '2', 'a', 11),
        accepted("1.0.0", '1', 'a', 10),
    ];
    let backend = FakeBackend::installed(history.clone(), pair("9.9.9", '9', '9'));

    LifecycleManager::new(&backend)
        .rollback(None)
        .expect("default rollback");
    assert_eq!(
        backend.state.borrow().accepted()[0]
            .pair
            .application
            .version,
        "1.1.0"
    );

    *backend.state.borrow_mut() = LifecycleState::installed(
        TargetHardwareIdentity {
            model: "Raspberry Pi Zero 2 W".into(),
            serial: "10000000abcdef01".into(),
        },
        history,
    )
    .unwrap();
    LifecycleManager::new(&backend)
        .rollback(Some(&Version::parse("1.0.0").unwrap()))
        .expect("explicit rollback");
    assert_eq!(
        backend.state.borrow().accepted()[0]
            .pair
            .application
            .version,
        "1.0.0"
    );
}

#[test]
fn absent_ambiguous_or_unaccepted_rollback_history_fails_closed() {
    let backend = FakeBackend::installed(vec![], pair("1.0.0", '1', 'a'));
    assert_eq!(
        LifecycleManager::new(&backend).rollback(None),
        Err(LifecycleError::NoAcceptedPair)
    );

    let backend = FakeBackend::installed(
        vec![accepted("1.0.0", '1', 'a', 1)],
        pair("2.0.0", '2', 'a'),
    );
    assert_eq!(
        LifecycleManager::new(&backend).rollback(None),
        Err(LifecycleError::NoPriorAcceptedPair)
    );
    assert_eq!(
        LifecycleManager::new(&backend).rollback(Some(&Version::parse("9.9.9").unwrap())),
        Err(LifecycleError::RequestedVersionNotAccepted)
    );
}

#[test]
fn a_task14_complete_state_migrates_deterministically_without_losing_owned_hashes() {
    let task14 = TargetInstallState {
        schema_version: 1,
        hardware: TargetHardwareIdentity {
            model: "Raspberry Pi Zero 2 W".into(),
            serial: "10000000abcdef01".into(),
        },
        application: Some(artifact("1.0.0", '1')),
        driver: Some(artifact("0.1.0", 'a')),
        owned_files: vec![OwnedFile {
            target_path: APP_PATH.into(),
            sha256: "1".repeat(64),
        }],
        last_verified_phase: planeradarctl::state::InstallPhase::Complete,
    };

    let migrated = LifecycleState::migrate_task14(&task14).expect("migration");

    assert_eq!(migrated.schema_version(), 1);
    assert_eq!(migrated.accepted().len(), 1);
    assert_eq!(migrated.accepted()[0].sequence, 1);
    assert_eq!(migrated.accepted()[0].owned_files, task14.owned_files);
}

#[test]
fn uninstall_uses_recorded_ownership_then_driver_and_is_idempotent() {
    let current = accepted("1.0.0", '1', 'a', 7);
    let backend = FakeBackend::installed(vec![current.clone()], pair("2.0.0", '2', 'a'));

    assert_eq!(
        LifecycleManager::new(&backend)
            .uninstall(false)
            .expect("first uninstall"),
        LifecycleOutcome::Uninstalled
    );
    assert_eq!(
        backend.calls.borrow().as_slice(),
        [
            Call::UninstallApplication,
            Call::UninstallDriver,
            Call::FinalizeUninstall,
        ]
    );
    assert!(backend.state.borrow().accepted().is_empty());

    backend.calls.borrow_mut().clear();
    assert_eq!(
        LifecycleManager::new(&backend)
            .uninstall(false)
            .expect("repeat uninstall"),
        LifecycleOutcome::AlreadyUninstalled
    );
    assert!(backend.calls.borrow().is_empty());
}

#[test]
fn purge_settings_is_passed_only_when_explicitly_requested() {
    struct PurgeBackend {
        inner: FakeBackend,
        purge: Cell<Option<bool>>,
    }
    impl LifecycleBackend for PurgeBackend {
        fn load_lifecycle_state(&self) -> Result<LifecycleState, LifecycleError> {
            self.inner.load_lifecycle_state()
        }
        fn save_lifecycle_state(&self, state: &LifecycleState) -> Result<(), LifecycleError> {
            self.inner.save_lifecycle_state(state)
        }
        fn resolve_release(
            &self,
            requested: Option<&Version>,
        ) -> Result<ReleasePair, LifecycleError> {
            self.inner.resolve_release(requested)
        }
        fn stage_application(&self, pair: &ReleasePair) -> Result<(), LifecycleError> {
            self.inner.stage_application(pair)
        }
        fn stage_driver(&self, pair: &ReleasePair) -> Result<(), LifecycleError> {
            self.inner.stage_driver(pair)
        }
        fn tryboot_driver(&self, pair: &ReleasePair) -> Result<(), LifecycleError> {
            self.inner.tryboot_driver(pair)
        }
        fn verify_tryboot_driver(&self, pair: &ReleasePair) -> Result<(), LifecycleError> {
            self.inner.verify_tryboot_driver(pair)
        }
        fn commit_driver(&self, pair: &ReleasePair) -> Result<(), LifecycleError> {
            self.inner.commit_driver(pair)
        }
        fn reboot_normal(&self, pair: &ReleasePair) -> Result<(), LifecycleError> {
            self.inner.reboot_normal(pair)
        }
        fn activate_application(
            &self,
            pair: &ReleasePair,
        ) -> Result<Vec<OwnedFile>, LifecycleError> {
            self.inner.activate_application(pair)
        }
        fn restart_application(&self) -> Result<(), LifecycleError> {
            self.inner.restart_application()
        }
        fn verify_pair(&self, pair: &ReleasePair) -> Result<(), LifecycleError> {
            self.inner.verify_pair(pair)
        }
        fn restore_application(&self, prior: &AcceptedPair) -> Result<(), LifecycleError> {
            self.inner.restore_application(prior)
        }
        fn restore_driver(&self, prior: &AcceptedPair) -> Result<(), LifecycleError> {
            self.inner.restore_driver(prior)
        }
        fn uninstall_application(
            &self,
            owned_files: &[OwnedFile],
            purge_settings: bool,
        ) -> Result<(), LifecycleError> {
            self.purge.set(Some(purge_settings));
            self.inner
                .uninstall_application(owned_files, purge_settings)
        }
        fn uninstall_driver(&self, driver: &ArtifactIdentity) -> Result<(), LifecycleError> {
            self.inner.uninstall_driver(driver)
        }
        fn finalize_uninstall(&self, state: &LifecycleState) -> Result<(), LifecycleError> {
            self.inner.finalize_uninstall(state)
        }
    }

    let backend = PurgeBackend {
        inner: FakeBackend::installed(
            vec![accepted("1.0.0", '1', 'a', 1)],
            pair("2.0.0", '2', 'a'),
        ),
        purge: Cell::new(None),
    };
    LifecycleManager::new(&backend)
        .uninstall(true)
        .expect("purge uninstall");
    assert_eq!(backend.purge.get(), Some(true));
}

#[test]
fn public_uninstall_accepts_an_explicit_purge_settings_flag() {
    let cli = Cli::try_parse_from([
        "planeradarctl",
        "uninstall",
        "pi@planeradar.local",
        "--purge-settings",
    ])
    .expect("parse uninstall");
    assert!(matches!(
        cli.command,
        CliCommand::Uninstall(options)
            if options.target.as_deref() == Some("pi@planeradar.local")
                && options.purge_settings
    ));
}

#[test]
fn public_lifecycle_commands_dispatch_and_return_typed_nonzero_target_failures() {
    for command in ["upgrade", "rollback", "uninstall"] {
        let temporary = tempfile::tempdir().expect("temporary working directory");
        let output = Command::new(env!("CARGO_BIN_EXE_planeradarctl"))
            .current_dir(temporary.path())
            .args([command, "not-a-user-at-host", "--non-interactive"])
            .output()
            .expect("run lifecycle command");
        assert!(!output.status.success(), "{command}");
        let stderr = String::from_utf8(output.stderr).expect("UTF-8 stderr");
        assert!(stderr.contains("SSH target"), "{command}: {stderr}");
        assert!(!stderr.contains("/Users/"), "{command}: {stderr}");
    }
}

#[test]
fn every_persisted_mutation_boundary_recovers_the_prior_pair_before_retrying() {
    let phases = [
        "prepared",
        "application_staged",
        "driver_staged",
        "tryboot_verified",
        "driver_committed",
        "normal_boot_verified",
        "application_activated",
        "application_restarted",
    ];
    for phase in phases {
        let prior = accepted("1.0.0", '1', 'a', 7);
        let candidate = pair("2.0.0", '2', 'b');
        let state = LifecycleState::from_json(
            serde_json::to_string(&serde_json::json!({
                "schema_version": 1,
                "hardware": {
                    "model": "Raspberry Pi Zero 2 W",
                    "serial": "10000000abcdef01"
                },
                "accepted": [prior.clone()],
                "transaction": {
                    "prior": prior,
                    "candidate": candidate.clone(),
                    "phase": phase
                }
            }))
            .unwrap()
            .as_bytes(),
        )
        .expect("persisted transaction");
        let backend = FakeBackend::installed(Vec::new(), candidate);
        *backend.state.borrow_mut() = state;

        LifecycleManager::new(&backend)
            .upgrade(None)
            .expect("recover then retry");

        assert_eq!(
            &backend.calls.borrow()[..5],
            [
                Call::Resolve,
                Call::RestoreDriver,
                Call::RestoreApplication,
                Call::RestartApplication,
                Call::VerifyPair,
            ],
            "{phase}"
        );
    }
}

#[test]
fn malformed_release_identity_is_rejected_before_any_target_mutation() {
    let current = accepted("1.0.0", '1', 'a', 7);
    let mut candidate = pair("2.0.0", '2', 'a');
    candidate.application.source_commit = "short".into();
    let backend = FakeBackend::installed(vec![current.clone()], candidate);

    assert_eq!(
        LifecycleManager::new(&backend).upgrade(None),
        Err(LifecycleError::ImmutableReleaseMismatch)
    );
    assert_eq!(backend.calls.borrow().as_slice(), [Call::Resolve]);
    assert_eq!(backend.state.borrow().accepted(), &[current]);
}

#[test]
fn explicit_rollback_rejects_two_distinct_accepted_identities_with_one_version() {
    let backend = FakeBackend::installed(
        vec![
            accepted("2.0.0", '4', 'a', 4),
            accepted("1.0.0", '3', 'a', 3),
            accepted("1.0.0", '2', 'a', 2),
        ],
        pair("9.0.0", '9', 'a'),
    );

    assert_eq!(
        LifecycleManager::new(&backend).rollback(Some(&Version::parse("1.0.0").unwrap())),
        Err(LifecycleError::AmbiguousAcceptedVersion)
    );
    assert!(backend.calls.borrow().is_empty());
}
