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
    VerifyHistorical,
    StageApplication,
    StageDriver,
    TrybootDriver,
    VerifyTryboot,
    CommitDriver,
    RebootNormal,
    ActivateApplication,
    RestartApplication,
    VerifyPair,
    FinalizeDriverAcceptance,
    RestoreApplication,
    RestoreDriver,
    RetireCandidate,
    PrepareUninstall,
    UninstallApplication,
    UninstallDriver,
    FinalizeDriverUninstall,
    FinalizeUninstall,
}

struct FakeBackend {
    state: RefCell<LifecycleState>,
    release: RefCell<ReleasePair>,
    owned: RefCell<Vec<OwnedFile>>,
    calls: RefCell<Vec<Call>>,
    fail: Cell<Option<Call>>,
    fail_save: Cell<Option<u32>>,
    save_count: Cell<u32>,
    requested: RefCell<Vec<Option<Version>>>,
    historical_expected: RefCell<Vec<ReleasePair>>,
    legacy_migration_expected: RefCell<Option<ReleasePair>>,
    staged_pairs: RefCell<Vec<ReleasePair>>,
    uninstall_drivers: RefCell<Vec<ArtifactIdentity>>,
    retired_candidates: RefCell<Vec<Vec<OwnedFile>>>,
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
            fail_save: Cell::new(None),
            save_count: Cell::new(0),
            requested: RefCell::new(Vec::new()),
            historical_expected: RefCell::new(Vec::new()),
            legacy_migration_expected: RefCell::new(None),
            staged_pairs: RefCell::new(Vec::new()),
            uninstall_drivers: RefCell::new(Vec::new()),
            retired_candidates: RefCell::new(Vec::new()),
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
        if let Some(expected) = self.legacy_migration_expected.borrow_mut().take() {
            self.verify_historical_release(&expected)?;
        }
        Ok(self.state.borrow().clone())
    }

    fn save_lifecycle_state(&self, state: &LifecycleState) -> Result<(), LifecycleError> {
        let count = self.save_count.get() + 1;
        self.save_count.set(count);
        if self.fail_save.get() == Some(count) {
            return Err(LifecycleError::Backend);
        }
        *self.state.borrow_mut() = state.clone();
        Ok(())
    }

    fn resolve_release(&self, requested: Option<&Version>) -> Result<ReleasePair, LifecycleError> {
        self.call(Call::Resolve)?;
        self.requested.borrow_mut().push(requested.cloned());
        Ok(self.release.borrow().clone())
    }

    fn verify_historical_release(&self, expected: &ReleasePair) -> Result<(), LifecycleError> {
        self.historical_expected.borrow_mut().push(expected.clone());
        self.call(Call::VerifyHistorical)
    }

    fn stage_application(&self, pair: &ReleasePair) -> Result<(), LifecycleError> {
        self.staged_pairs.borrow_mut().push(pair.clone());
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

    fn finalize_driver_acceptance(&self, _pair: &ReleasePair) -> Result<(), LifecycleError> {
        self.call(Call::FinalizeDriverAcceptance)
    }

    fn restore_application(&self, _prior: &AcceptedPair) -> Result<Vec<OwnedFile>, LifecycleError> {
        self.call(Call::RestoreApplication)?;
        Ok(self.owned.borrow().clone())
    }

    fn restore_driver(&self, _prior: &AcceptedPair) -> Result<(), LifecycleError> {
        self.call(Call::RestoreDriver)
    }

    fn retire_candidate(&self, owned_files: &[OwnedFile]) -> Result<(), LifecycleError> {
        self.retired_candidates
            .borrow_mut()
            .push(owned_files.to_vec());
        self.call(Call::RetireCandidate)
    }

    fn prepare_uninstall(&self, accepted: &AcceptedPair) -> Result<OwnedFile, LifecycleError> {
        self.call(Call::PrepareUninstall)?;
        Ok(OwnedFile {
            target_path: format!(
                "/var/lib/planeradar-installer/helpers/{}/planeradar",
                accepted.pair.application.sha256
            ),
            sha256: accepted.pair.application.sha256.clone(),
        })
    }

    fn uninstall_application(
        &self,
        _owned_files: &[OwnedFile],
        _purge_settings: bool,
    ) -> Result<(), LifecycleError> {
        self.call(Call::UninstallApplication)
    }

    fn uninstall_driver(&self, drivers: &[ArtifactIdentity]) -> Result<(), LifecycleError> {
        *self.uninstall_drivers.borrow_mut() = drivers.to_vec();
        self.call(Call::UninstallDriver)
    }

    fn finalize_driver_uninstall(&self) -> Result<(), LifecycleError> {
        self.call(Call::FinalizeDriverUninstall)
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
        let calls = backend.calls.borrow();
        let restore = calls
            .iter()
            .position(|call| *call == Call::RestoreApplication)
            .expect("application restored");
        assert_eq!(calls[restore], Call::RestoreApplication);
        assert_eq!(calls[restore + 1], Call::RestartApplication);
        if failure == Call::ActivateApplication {
            assert_eq!(
                &calls[restore..],
                [
                    Call::RestoreApplication,
                    Call::RestartApplication,
                    Call::VerifyPair,
                    Call::RetireCandidate,
                ],
            );
            assert_eq!(backend.state.borrow().accepted()[0].pair, current.pair);
            assert_eq!(
                backend.state.borrow().accepted()[0].owned_files,
                backend.owned.borrow().clone()
            );
        } else {
            assert!(
                backend
                    .state
                    .borrow()
                    .to_json()
                    .expect("resumable recovery")
                    .contains("\"transaction\":{"),
                "{failure:?}"
            );
        }
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
            Call::FinalizeDriverAcceptance,
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
        Call::RetireCandidate,
    ]));
    assert_eq!(backend.state.borrow().accepted()[0].pair, current.pair);
    assert_eq!(
        backend.state.borrow().accepted()[0].owned_files,
        backend.owned.borrow().clone()
    );
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
        history.clone(),
    )
    .unwrap();
    *backend.release.borrow_mut() = pair("1.0.0", '1', 'a');
    backend.calls.borrow_mut().clear();
    backend.requested.borrow_mut().clear();
    backend.historical_expected.borrow_mut().clear();
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
    assert_eq!(
        backend.calls.borrow().first(),
        Some(&Call::VerifyHistorical)
    );
    assert_eq!(
        backend.historical_expected.borrow().as_slice(),
        &[history[2].pair.clone()]
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

    assert_eq!(migrated.schema_version(), 3);
    assert_eq!(migrated.accepted().len(), 1);
    assert_eq!(migrated.accepted()[0].sequence, 1);
    assert_eq!(migrated.accepted()[0].owned_files, task14.owned_files);
}

#[test]
fn first_upgrade_migrates_and_attests_the_task14_pair_before_resolving_the_rc_candidate() {
    let task14_pair = pair("1.0.0", '1', 'a');
    let rc_candidate = pair("2.0.0", '2', 'b');
    let prior = AcceptedPair {
        pair: task14_pair.clone(),
        sequence: 1,
        owned_files: vec![OwnedFile {
            target_path: APP_PATH.into(),
            sha256: task14_pair.application.sha256.clone(),
        }],
    };
    let backend = FakeBackend::installed(vec![prior], rc_candidate.clone());
    *backend.legacy_migration_expected.borrow_mut() = Some(task14_pair.clone());

    LifecycleManager::new(&backend)
        .upgrade(None)
        .expect("first post-Task-14 upgrade");

    assert_eq!(
        &backend.calls.borrow()[..2],
        [Call::VerifyHistorical, Call::Resolve]
    );
    assert_eq!(
        backend.historical_expected.borrow().as_slice(),
        &[task14_pair]
    );
    assert_eq!(
        backend.staged_pairs.borrow().as_slice(),
        &[rc_candidate.clone()]
    );
    assert_eq!(backend.state.borrow().accepted()[0].pair, rc_candidate);
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
            Call::PrepareUninstall,
            Call::UninstallApplication,
            Call::UninstallDriver,
            Call::FinalizeDriverUninstall,
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
fn uninstall_removes_each_unique_accepted_driver_in_current_first_order() {
    let current = accepted("1.2.0", '3', 'b', 12);
    let duplicate_current_driver = accepted("1.1.0", '2', 'b', 11);
    let prior_driver = accepted("1.0.0", '1', 'a', 10);
    let backend = FakeBackend::installed(
        vec![
            current.clone(),
            duplicate_current_driver,
            prior_driver.clone(),
        ],
        pair("2.0.0", '4', 'c'),
    );

    LifecycleManager::new(&backend)
        .uninstall(false)
        .expect("uninstall accepted driver history");

    assert_eq!(
        backend.uninstall_drivers.borrow().as_slice(),
        [current.pair.driver, prior_driver.pair.driver,]
    );
}

#[test]
fn uninstall_resumes_after_every_destructive_and_persistence_boundary() {
    enum Boundary {
        Action(Call),
        Save(u32),
    }
    for boundary in [
        Boundary::Action(Call::UninstallApplication),
        Boundary::Save(2),
        Boundary::Action(Call::UninstallDriver),
        Boundary::Save(3),
        Boundary::Action(Call::FinalizeUninstall),
    ] {
        let current = accepted("1.0.0", '1', 'a', 7);
        let backend = FakeBackend::installed(vec![current], pair("2.0.0", '2', 'a'));
        match boundary {
            Boundary::Action(call) => backend.fail.set(Some(call)),
            Boundary::Save(save) => backend.fail_save.set(Some(save)),
        }
        assert_eq!(
            LifecycleManager::new(&backend).uninstall(false),
            Err(LifecycleError::Backend)
        );

        backend.fail.set(None);
        backend.fail_save.set(None);
        backend.calls.borrow_mut().clear();
        assert_eq!(
            LifecycleManager::new(&backend)
                .uninstall(false)
                .expect("resume uninstall"),
            LifecycleOutcome::Uninstalled
        );
        assert!(backend.state.borrow().accepted().is_empty());
    }
}

#[test]
fn uninstall_retry_rejects_changed_purge_intent_before_more_deletion() {
    let current = accepted("1.0.0", '1', 'a', 7);
    let backend = FakeBackend::installed(vec![current], pair("2.0.0", '2', 'a'));
    backend.fail.set(Some(Call::UninstallApplication));
    assert_eq!(
        LifecycleManager::new(&backend).uninstall(false),
        Err(LifecycleError::Backend)
    );
    backend.fail.set(None);
    backend.calls.borrow_mut().clear();

    assert_eq!(
        LifecycleManager::new(&backend).uninstall(true),
        Err(LifecycleError::UninstallOptionsMismatch)
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
        fn verify_historical_release(&self, expected: &ReleasePair) -> Result<(), LifecycleError> {
            self.inner.verify_historical_release(expected)
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
        fn finalize_driver_acceptance(&self, pair: &ReleasePair) -> Result<(), LifecycleError> {
            self.inner.finalize_driver_acceptance(pair)
        }
        fn restore_application(
            &self,
            prior: &AcceptedPair,
        ) -> Result<Vec<OwnedFile>, LifecycleError> {
            self.inner.restore_application(prior)
        }
        fn restore_driver(&self, prior: &AcceptedPair) -> Result<(), LifecycleError> {
            self.inner.restore_driver(prior)
        }
        fn retire_candidate(&self, owned_files: &[OwnedFile]) -> Result<(), LifecycleError> {
            self.inner.retire_candidate(owned_files)
        }

        fn prepare_uninstall(&self, accepted: &AcceptedPair) -> Result<OwnedFile, LifecycleError> {
            self.inner.prepare_uninstall(accepted)
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
        fn uninstall_driver(&self, drivers: &[ArtifactIdentity]) -> Result<(), LifecycleError> {
            self.inner.uninstall_driver(drivers)
        }
        fn finalize_driver_uninstall(&self) -> Result<(), LifecycleError> {
            self.inner.finalize_driver_uninstall()
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
            &backend.calls.borrow()[..6],
            [
                Call::RestoreDriver,
                Call::RestoreApplication,
                Call::RestartApplication,
                Call::VerifyPair,
                Call::RetireCandidate,
                Call::Resolve,
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
fn explicit_rollback_rejects_a_verified_release_that_does_not_match_accepted_identity() {
    let backend = FakeBackend::installed(
        vec![
            accepted("2.0.0", '4', 'a', 4),
            accepted("1.0.0", '3', 'a', 3),
            accepted("1.0.0", '2', 'a', 2),
        ],
        pair("1.0.0", '9', 'a'),
    );

    assert_eq!(
        LifecycleManager::new(&backend).rollback(Some(&Version::parse("1.0.0").unwrap())),
        Err(LifecycleError::ImmutableReleaseMismatch)
    );
    assert_eq!(backend.calls.borrow().as_slice(), []);
}

#[test]
fn explicit_old_driver_rollback_verifies_the_exact_historical_pair_without_current_lock_resolution()
{
    let current = accepted("2.0.0", '2', 'b', 2);
    let historical = accepted("1.0.0", '1', 'a', 1);
    let backend =
        FakeBackend::installed(vec![current, historical.clone()], pair("9.9.9", '9', 'f'));

    LifecycleManager::new(&backend)
        .rollback(Some(&Version::parse("1.0.0").unwrap()))
        .expect("verified old-driver rollback");

    assert_eq!(
        backend.historical_expected.borrow().as_slice(),
        &[historical.pair]
    );
    assert!(!backend.calls.borrow().contains(&Call::Resolve));
}

#[test]
fn app_acceptance_save_failure_restores_prior_and_retires_exact_candidate_assets() {
    let prior = accepted("1.0.0", '1', 'a', 1);
    let candidate = pair("2.0.0", '2', 'b');
    let backend = FakeBackend::installed(vec![prior.clone()], candidate.clone());
    backend.fail_save.set(Some(9));

    assert_eq!(
        LifecycleManager::new(&backend).upgrade(None),
        Err(LifecycleError::Backend)
    );
    assert!(
        !backend
            .calls
            .borrow()
            .contains(&Call::FinalizeDriverAcceptance)
    );
    assert!(backend.calls.borrow().contains(&Call::RestoreDriver));
    assert_eq!(backend.state.borrow().accepted()[0].pair, prior.pair);
    assert_eq!(
        backend.retired_candidates.borrow().last().unwrap(),
        &vec![
            OwnedFile {
                target_path: format!(
                    "/opt/planeradar/releases/{}/{}/planeradar",
                    candidate.application.version, candidate.application.sha256
                ),
                sha256: candidate.application.sha256.clone(),
            },
            OwnedFile {
                target_path: format!(
                    "/var/lib/planeradar-installer/helpers/{}/planeradar",
                    candidate.application.sha256
                ),
                sha256: candidate.application.sha256,
            },
        ]
    );
}

#[test]
fn crash_after_durable_app_acceptance_finalizes_candidate_instead_of_rolling_back() {
    let prior = accepted("1.0.0", '1', 'a', 1);
    let candidate_pair = pair("2.0.0", '2', 'b');
    let candidate = AcceptedPair {
        pair: candidate_pair.clone(),
        sequence: 2,
        owned_files: vec![OwnedFile {
            target_path: APP_PATH.into(),
            sha256: "2".repeat(64),
        }],
    };
    let release_path = format!(
        "/opt/planeradar/releases/{}/{}/planeradar",
        candidate_pair.application.version, candidate_pair.application.sha256
    );
    let helper_path = format!(
        "/var/lib/planeradar-installer/helpers/{}/planeradar",
        candidate_pair.application.sha256
    );
    let state = LifecycleState::from_json(
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 3,
            "hardware": {
                "model": "Raspberry Pi Zero 2 W",
                "serial": "10000000abcdef01"
            },
            "accepted": [candidate, prior.clone()],
            "transaction": {
                "prior": prior,
                "candidate": candidate_pair.clone(),
                "candidate_owned_files": [
                    {
                        "target_path": release_path,
                        "sha256": candidate_pair.application.sha256
                    },
                    {
                        "target_path": helper_path,
                        "sha256": candidate_pair.application.sha256
                    }
                ],
                "restored_owned_files": null,
                "phase": "pair_accepted"
            },
            "uninstall": null
        }))
        .unwrap()
        .as_slice(),
    )
    .expect("durable pair acceptance");
    let backend = FakeBackend::installed(Vec::new(), candidate_pair);
    *backend.state.borrow_mut() = state;

    LifecycleManager::new(&backend)
        .upgrade(None)
        .expect("resume accepted candidate");

    assert_eq!(
        backend.calls.borrow().first(),
        Some(&Call::FinalizeDriverAcceptance)
    );
    assert!(!backend.calls.borrow().contains(&Call::RestoreDriver));
}

#[test]
fn repeated_failed_candidates_are_exactly_retired_and_never_expand_accepted_history() {
    let prior = accepted("1.0.0", '1', 'a', 1);
    let backend = FakeBackend::installed(vec![prior.clone()], pair("2.0.0", '2', 'a'));
    backend.fail.set(Some(Call::ActivateApplication));

    for (version, seed) in [
        ("2.0.0", '2'),
        ("3.0.0", '3'),
        ("4.0.0", '4'),
        ("5.0.0", '5'),
        ("6.0.0", '6'),
    ] {
        *backend.release.borrow_mut() = pair(version, seed, 'a');
        assert_eq!(
            LifecycleManager::new(&backend).upgrade(None),
            Err(LifecycleError::Backend)
        );
    }

    assert_eq!(backend.state.borrow().accepted().len(), 1);
    assert_eq!(backend.state.borrow().accepted()[0].pair, prior.pair);
    let retired = backend.retired_candidates.borrow();
    assert_eq!(retired.len(), 5);
    assert!(retired.iter().all(|candidate| {
        candidate.len() == 2
            && candidate[0]
                .target_path
                .starts_with("/opt/planeradar/releases/")
            && candidate[1]
                .target_path
                .starts_with("/var/lib/planeradar-installer/helpers/")
    }));
}
