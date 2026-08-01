use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fs;
use std::io::Cursor;
use std::os::unix::fs::PermissionsExt;
use std::rc::Rc;

use planeradarctl::install::{
    BackendFailure, InstallBackend, InstallOutcome, InstallRequest, InstallStatusEvent, Installer,
    InterruptionReason, PhaseVerification, TRYBOOT_TIMEOUT_GUIDANCE, TargetApplicationInstall,
    TargetInstallOwnership, TargetInstallResult, extract_application_payload,
};
use planeradarctl::state::{
    ArtifactIdentity, InstallPhase, InstallState, OwnedFile, STATE_SCHEMA_VERSION, StateError,
    StateStore, TargetHardwareIdentity, TargetInstallState, TargetStateStore,
};
use planeradarctl::target::TargetIdentity;
use sha2::{Digest, Sha256};

#[derive(Clone, Default)]
struct MemoryStateStore {
    state: Rc<RefCell<Option<InstallState>>>,
}

impl StateStore for MemoryStateStore {
    fn load(&self) -> Result<Option<InstallState>, StateError> {
        Ok(self.state.borrow().clone())
    }

    fn save(&self, state: &InstallState) -> Result<(), StateError> {
        *self.state.borrow_mut() = Some(state.clone());
        Ok(())
    }
}

#[derive(Clone, Default)]
struct ScriptedBackend {
    state: Rc<RefCell<BackendState>>,
}

#[derive(Default)]
struct BackendState {
    target_state: Option<TargetInstallState>,
    actions: Vec<InstallPhase>,
    events: Vec<InstallStatusEvent>,
    verifications: Vec<InstallPhase>,
    drifted: BTreeSet<InstallPhase>,
    fail_action: Option<(InstallPhase, BackendFailure)>,
    fail_verification: Option<(InstallPhase, BackendFailure)>,
    interrupt_after: Option<InstallPhase>,
    hostname_reconnects: Vec<(TargetIdentity, String)>,
}

impl ScriptedBackend {
    fn actions(&self) -> Vec<InstallPhase> {
        self.state.borrow().actions.clone()
    }

    fn events(&self) -> Vec<InstallStatusEvent> {
        self.state.borrow().events.clone()
    }

    fn target_state(&self) -> Option<TargetInstallState> {
        self.state.borrow().target_state.clone()
    }

    fn fail_action_once(&self, phase: InstallPhase, failure: BackendFailure) {
        self.state.borrow_mut().fail_action = Some((phase, failure));
    }

    fn fail_verification_once(&self, phase: InstallPhase, failure: BackendFailure) {
        self.state.borrow_mut().fail_verification = Some((phase, failure));
    }

    fn interrupt_after(&self, phase: InstallPhase) {
        self.state.borrow_mut().interrupt_after = Some(phase);
    }

    fn drift(&self, phase: InstallPhase) {
        self.state.borrow_mut().drifted.insert(phase);
    }

    fn record_action(&self, phase: InstallPhase) -> Result<(), BackendFailure> {
        let mut state = self.state.borrow_mut();
        state.actions.push(phase);
        if state
            .fail_action
            .as_ref()
            .is_some_and(|(failed, _)| *failed == phase)
        {
            return Err(state.fail_action.take().expect("present").1);
        }
        Ok(())
    }
}

impl TargetStateStore for ScriptedBackend {
    fn load_target_state(&self) -> Result<Option<TargetInstallState>, StateError> {
        Ok(self.state.borrow().target_state.clone())
    }

    fn save_target_state(&self, state: &TargetInstallState) -> Result<(), StateError> {
        state.to_json()?;
        self.state.borrow_mut().target_state = Some(state.clone());
        Ok(())
    }
}

impl InstallBackend for ScriptedBackend {
    fn discover(&self, request: &InstallRequest) -> Result<TargetIdentity, BackendFailure> {
        self.record_action(InstallPhase::Discovered)?;
        Ok(request.target.clone())
    }

    fn run_preflight(&self, _request: &InstallRequest) -> Result<(), BackendFailure> {
        self.record_action(InstallPhase::PreflightPassed)
    }

    fn acquire_application(
        &self,
        request: &InstallRequest,
    ) -> Result<ArtifactIdentity, BackendFailure> {
        self.record_action(InstallPhase::ApplicationAcquired)?;
        Ok(request.application.clone())
    }

    fn prepare_driver(&self, request: &InstallRequest) -> Result<ArtifactIdentity, BackendFailure> {
        self.record_action(InstallPhase::DriverReady)?;
        Ok(request.driver.clone())
    }

    fn stage_tryboot(&self, _request: &InstallRequest) -> Result<(), BackendFailure> {
        self.record_action(InstallPhase::TrybootStaged)
    }

    fn boot_and_verify_tryboot(&self, _request: &InstallRequest) -> Result<(), BackendFailure> {
        self.record_action(InstallPhase::TrybootVerified)
    }

    fn accept_driver(&self, _request: &InstallRequest) -> Result<(), BackendFailure> {
        self.record_action(InstallPhase::DriverAccepted)
    }

    fn install_application(
        &self,
        request: &InstallRequest,
    ) -> Result<TargetApplicationInstall, BackendFailure> {
        self.record_action(InstallPhase::ApplicationInstalled)?;
        Ok(TargetApplicationInstall {
            result: TargetInstallResult {
                schema_version: 1,
                files_changed: true,
                boot_config_changed: false,
                reboot_required: false,
                revision: request.application.source_commit.clone(),
                sha256: request.application.sha256.clone(),
            },
            ownership: TargetInstallOwnership {
                schema_version: 1,
                owned_files: expected_owned_files(&request.application),
            },
        })
    }

    fn change_hostname_and_reconnect(
        &self,
        expected_identity: &TargetIdentity,
        desired_hostname: &str,
    ) -> Result<(), BackendFailure> {
        self.record_action(InstallPhase::HostnameChanged)?;
        self.state
            .borrow_mut()
            .hostname_reconnects
            .push((expected_identity.clone(), desired_hostname.to_owned()));
        Ok(())
    }

    fn reboot_final(&self, _request: &InstallRequest) -> Result<(), BackendFailure> {
        self.record_action(InstallPhase::FinalRebooted)
    }

    fn verify_final_service(&self, _request: &InstallRequest) -> Result<(), BackendFailure> {
        self.record_action(InstallPhase::FinalVerified)
    }

    fn finish(&self, _request: &InstallRequest) -> Result<(), BackendFailure> {
        self.record_action(InstallPhase::Complete)
    }

    fn verify_phase(
        &self,
        phase: InstallPhase,
        _request: &InstallRequest,
        _state: &InstallState,
    ) -> Result<PhaseVerification, BackendFailure> {
        let mut state = self.state.borrow_mut();
        state.verifications.push(phase);
        if state
            .fail_verification
            .as_ref()
            .is_some_and(|(failed, _)| *failed == phase)
        {
            return Err(state.fail_verification.take().expect("present").1);
        }
        Ok(if state.drifted.remove(&phase) {
            PhaseVerification::Drifted
        } else {
            PhaseVerification::Valid
        })
    }

    fn emit_status(&self, event: InstallStatusEvent) -> Result<(), BackendFailure> {
        let mut state = self.state.borrow_mut();
        state.events.push(event.clone());
        if state.interrupt_after == Some(event.phase) {
            state.interrupt_after = None;
            return Err(BackendFailure::MacProcessInterrupted);
        }
        Ok(())
    }
}

fn target() -> TargetIdentity {
    TargetIdentity {
        host_key_sha256: format!("SHA256:{}", "a".repeat(43)),
        model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
        serial: "0123456789abcdef".into(),
    }
}

fn application() -> ArtifactIdentity {
    ArtifactIdentity {
        version: "1.2.3".into(),
        source_commit: "1".repeat(40),
        sha256: "2".repeat(64),
    }
}

fn driver() -> ArtifactIdentity {
    ArtifactIdentity {
        version: "0.1.0".into(),
        source_commit: "3".repeat(40),
        sha256: "4".repeat(64),
    }
}

fn request() -> InstallRequest {
    InstallRequest {
        target: target(),
        application: application(),
        driver: driver(),
        desired_hostname: "planeradar".into(),
    }
}

fn expected_state(phase: InstallPhase) -> InstallState {
    InstallState {
        schema_version: STATE_SCHEMA_VERSION,
        target: target(),
        phase,
        application: (phase >= InstallPhase::ApplicationAcquired).then(application),
        driver: (phase >= InstallPhase::DriverReady).then(driver),
    }
}

fn expected_target_state(phase: InstallPhase) -> TargetInstallState {
    let state = expected_state(phase);
    TargetInstallState {
        schema_version: STATE_SCHEMA_VERSION,
        hardware: TargetHardwareIdentity {
            model: state.target.model,
            serial: state.target.serial,
        },
        application: state.application,
        driver: state.driver,
        owned_files: if phase >= InstallPhase::ApplicationInstalled {
            expected_owned_files(&application())
        } else {
            vec![]
        },
        last_verified_phase: phase,
    }
}

fn expected_owned_files(application: &ArtifactIdentity) -> Vec<OwnedFile> {
    [
        ("/opt/planeradar/bin/planeradar", application.sha256.clone()),
        ("/opt/planeradar/REVISION", "7".repeat(64)),
        ("/opt/planeradar/SHA256", "8".repeat(64)),
        ("/etc/systemd/system/planeradar.service", "9".repeat(64)),
        ("/var/lib/planeradar/settings.json", "a".repeat(64)),
        (
            "/var/lib/planeradar-installer/settings-owned-v1",
            "b".repeat(64),
        ),
    ]
    .into_iter()
    .map(|(target_path, sha256)| OwnedFile {
        target_path: target_path.into(),
        sha256,
    })
    .collect()
}

fn assert_records_agree(store: &MemoryStateStore, backend: &ScriptedBackend, phase: InstallPhase) {
    assert_eq!(
        store.load().expect("Mac state"),
        Some(expected_state(phase))
    );
    if phase <= InstallPhase::PreflightPassed {
        assert_eq!(backend.target_state(), None);
    } else {
        assert_eq!(backend.target_state(), Some(expected_target_state(phase)));
    }
}

#[test]
fn scripted_happy_path_persists_and_emits_the_exact_twelve_phase_sequence() {
    let backend = ScriptedBackend::default();
    let store = MemoryStateStore::default();

    let outcome = Installer::new(&backend, &store)
        .run(request())
        .expect("install");

    assert_eq!(outcome, InstallOutcome::Complete);
    assert_eq!(backend.actions(), InstallPhase::ALL);
    assert_eq!(
        backend
            .events()
            .iter()
            .map(|event| event.phase)
            .collect::<Vec<_>>(),
        InstallPhase::ALL
    );
    assert_records_agree(&store, &backend, InstallPhase::Complete);
}

#[test]
fn every_persisted_phase_is_an_idempotent_resume_boundary() {
    for interrupted_after in InstallPhase::ALL {
        let backend = ScriptedBackend::default();
        let store = MemoryStateStore::default();
        backend.interrupt_after(interrupted_after);

        let first = Installer::new(&backend, &store)
            .run(request())
            .expect("scripted interruption");
        assert_eq!(
            first,
            InstallOutcome::Interrupted {
                phase: interrupted_after,
                reason: InterruptionReason::MacProcessInterrupted,
                guidance: None,
            },
            "{interrupted_after:?}"
        );
        assert_records_agree(&store, &backend, interrupted_after);
        let before_resume = backend.actions();

        let second = Installer::new(&backend, &store)
            .run(request())
            .expect("resume");
        let expected = if interrupted_after == InstallPhase::Complete {
            InstallOutcome::AlreadyComplete
        } else {
            InstallOutcome::Complete
        };
        assert_eq!(second, expected, "{interrupted_after:?}");
        assert_eq!(
            &backend.actions()[..before_resume.len()],
            before_resume.as_slice(),
            "{interrupted_after:?}"
        );
        for completed in InstallPhase::ALL
            .into_iter()
            .take_while(|phase| *phase <= interrupted_after)
        {
            assert_eq!(
                backend
                    .actions()
                    .iter()
                    .filter(|phase| **phase == completed)
                    .count(),
                1,
                "{interrupted_after:?} repeated {completed:?}"
            );
        }
        assert_records_agree(&store, &backend, InstallPhase::Complete);
    }
}

#[test]
fn drift_at_each_phase_repeats_only_that_phase_and_its_successors() {
    for drifted in InstallPhase::ALL {
        let backend = ScriptedBackend::default();
        let store = MemoryStateStore::default();
        store
            .save(&expected_state(InstallPhase::Complete))
            .expect("seed Mac state");
        backend
            .save_target_state(&expected_target_state(InstallPhase::Complete))
            .expect("seed target state");
        backend.drift(drifted);

        let outcome = Installer::new(&backend, &store)
            .run(request())
            .expect("resume from drift");

        assert_eq!(outcome, InstallOutcome::Complete, "{drifted:?}");
        assert_eq!(
            backend.actions(),
            InstallPhase::ALL
                .into_iter()
                .filter(|phase| *phase >= drifted)
                .collect::<Vec<_>>(),
            "{drifted:?}"
        );
        assert_records_agree(&store, &backend, InstallPhase::Complete);
    }
}

#[test]
fn resume_refuses_disagreeing_mac_and_target_records_before_any_operation() {
    let cases = [
        (
            Some(expected_state(InstallPhase::DriverReady)),
            Some(expected_target_state(InstallPhase::ApplicationAcquired)),
        ),
        (Some(expected_state(InstallPhase::DriverReady)), None),
        (None, Some(expected_target_state(InstallPhase::DriverReady))),
        (
            Some(expected_state(InstallPhase::DriverReady)),
            Some(TargetInstallState {
                hardware: TargetHardwareIdentity {
                    serial: "fedcba9876543210".into(),
                    ..expected_target_state(InstallPhase::DriverReady).hardware
                },
                ..expected_target_state(InstallPhase::DriverReady)
            }),
        ),
        (
            Some(expected_state(InstallPhase::DriverReady)),
            Some(TargetInstallState {
                application: Some(ArtifactIdentity {
                    sha256: "5".repeat(64),
                    ..application()
                }),
                ..expected_target_state(InstallPhase::DriverReady)
            }),
        ),
    ];

    for (mac, target_state) in cases {
        let backend = ScriptedBackend::default();
        let store = MemoryStateStore::default();
        *store.state.borrow_mut() = mac;
        backend.state.borrow_mut().target_state = target_state;

        let error = Installer::new(&backend, &store)
            .run(request())
            .expect_err("disagreement must block");

        assert!(error.is_state_disagreement());
        assert!(backend.actions().is_empty());
        assert!(backend.state.borrow().verifications.is_empty());
    }
}

#[test]
fn target_record_one_phase_ahead_is_verified_then_reconciled_without_repeating_the_action() {
    let backend = ScriptedBackend::default();
    let store = MemoryStateStore::default();
    store
        .save(&expected_state(InstallPhase::ApplicationAcquired))
        .expect("seed Mac state");
    backend
        .save_target_state(&expected_target_state(InstallPhase::DriverReady))
        .expect("seed ahead target state");

    let outcome = Installer::new(&backend, &store)
        .run(request())
        .expect("reconcile");

    assert_eq!(outcome, InstallOutcome::Complete);
    assert_eq!(
        backend
            .actions()
            .iter()
            .filter(|phase| **phase == InstallPhase::DriverReady)
            .count(),
        0
    );
    assert_records_agree(&store, &backend, InstallPhase::Complete);
}

#[test]
fn target_record_multiple_phases_ahead_is_reconciled_only_after_every_intermediate_postcondition() {
    let backend = ScriptedBackend::default();
    let store = MemoryStateStore::default();
    store
        .save(&expected_state(InstallPhase::Discovered))
        .expect("seed Mac state");
    backend
        .save_target_state(&expected_target_state(InstallPhase::DriverReady))
        .expect("seed ahead target state");

    let outcome = Installer::new(&backend, &store)
        .run(request())
        .expect("reconcile multiple durable target phases");

    assert_eq!(outcome, InstallOutcome::Complete);
    for reconciled in [
        InstallPhase::PreflightPassed,
        InstallPhase::ApplicationAcquired,
        InstallPhase::DriverReady,
    ] {
        assert_eq!(
            backend
                .actions()
                .iter()
                .filter(|phase| **phase == reconciled)
                .count(),
            0,
            "repeated reconciled phase {reconciled:?}"
        );
    }
    assert_records_agree(&store, &backend, InstallPhase::Complete);
}

#[test]
fn target_record_multiple_phases_ahead_is_rejected_when_any_intermediate_postcondition_drifted() {
    let backend = ScriptedBackend::default();
    let store = MemoryStateStore::default();
    store
        .save(&expected_state(InstallPhase::Discovered))
        .expect("seed Mac state");
    backend
        .save_target_state(&expected_target_state(InstallPhase::DriverReady))
        .expect("seed ahead target state");
    backend.drift(InstallPhase::ApplicationAcquired);

    let error = Installer::new(&backend, &store)
        .run(request())
        .expect_err("drifted intermediate phase must block reconciliation");

    assert!(error.is_state_disagreement());
    assert_eq!(
        store
            .load()
            .expect("load Mac state")
            .expect("Mac state")
            .phase,
        InstallPhase::Discovered
    );
    assert!(backend.actions().is_empty());
}

#[test]
fn a_postcondition_failure_never_persists_the_unverified_phase() {
    let backend = ScriptedBackend::default();
    let store = MemoryStateStore::default();
    backend.drift(InstallPhase::DriverReady);

    let outcome = Installer::new(&backend, &store)
        .run(request())
        .expect("postcondition outcome");

    assert_eq!(
        outcome,
        InstallOutcome::Interrupted {
            phase: InstallPhase::ApplicationAcquired,
            reason: InterruptionReason::PostconditionFailed(InstallPhase::DriverReady),
            guidance: None,
        }
    );
    assert_records_agree(&store, &backend, InstallPhase::ApplicationAcquired);
}

#[test]
fn ssh_loss_preserves_the_last_agreed_phase_for_resume() {
    let backend = ScriptedBackend::default();
    let store = MemoryStateStore::default();
    backend.fail_action_once(InstallPhase::DriverAccepted, BackendFailure::SshLost);

    let outcome = Installer::new(&backend, &store)
        .run(request())
        .expect("SSH interruption");

    assert_eq!(
        outcome,
        InstallOutcome::Interrupted {
            phase: InstallPhase::TrybootVerified,
            reason: InterruptionReason::SshLost,
            guidance: None,
        }
    );
    assert_records_agree(&store, &backend, InstallPhase::TrybootVerified);

    assert_eq!(
        Installer::new(&backend, &store)
            .run(request())
            .expect("resume"),
        InstallOutcome::Complete
    );
    assert_eq!(
        backend
            .actions()
            .iter()
            .filter(|phase| **phase == InstallPhase::TrybootVerified)
            .count(),
        1
    );
}

#[test]
fn tryboot_timeout_keeps_staged_state_and_reports_fallback_recovery() {
    let backend = ScriptedBackend::default();
    let store = MemoryStateStore::default();
    backend.fail_action_once(
        InstallPhase::TrybootVerified,
        BackendFailure::TrybootTimedOut,
    );

    let outcome = Installer::new(&backend, &store)
        .run(request())
        .expect("tryboot timeout");

    assert_eq!(
        outcome,
        InstallOutcome::Interrupted {
            phase: InstallPhase::TrybootStaged,
            reason: InterruptionReason::TrybootTimedOut,
            guidance: Some(TRYBOOT_TIMEOUT_GUIDANCE),
        }
    );
    assert_records_agree(&store, &backend, InstallPhase::TrybootStaged);
}

#[test]
fn tryboot_verification_failure_does_not_accept_the_driver() {
    let backend = ScriptedBackend::default();
    let store = MemoryStateStore::default();
    backend.fail_verification_once(
        InstallPhase::TrybootVerified,
        BackendFailure::TrybootVerificationFailed,
    );

    let outcome = Installer::new(&backend, &store)
        .run(request())
        .expect("tryboot verification failure");

    assert_eq!(
        outcome,
        InstallOutcome::Interrupted {
            phase: InstallPhase::TrybootStaged,
            reason: InterruptionReason::TrybootVerificationFailed,
            guidance: None,
        }
    );
    assert_records_agree(&store, &backend, InstallPhase::TrybootStaged);
    assert!(!backend.actions().contains(&InstallPhase::DriverAccepted));
}

#[test]
fn final_service_failure_keeps_the_final_reboot_resume_point() {
    let backend = ScriptedBackend::default();
    let store = MemoryStateStore::default();
    backend.fail_action_once(
        InstallPhase::FinalVerified,
        BackendFailure::FinalServiceFailed,
    );

    let outcome = Installer::new(&backend, &store)
        .run(request())
        .expect("final service failure");

    assert_eq!(
        outcome,
        InstallOutcome::Interrupted {
            phase: InstallPhase::FinalRebooted,
            reason: InterruptionReason::FinalServiceFailed,
            guidance: None,
        }
    );
    assert_records_agree(&store, &backend, InstallPhase::FinalRebooted);
}

#[test]
fn hostname_change_reconnect_is_bound_to_the_persisted_identity() {
    let backend = ScriptedBackend::default();
    let store = MemoryStateStore::default();

    Installer::new(&backend, &store)
        .run(request())
        .expect("install");

    assert_eq!(
        backend.state.borrow().hostname_reconnects,
        vec![(target(), "planeradar".into())]
    );
}

#[test]
fn completed_rerun_verifies_but_repeats_no_actions_or_events() {
    let backend = ScriptedBackend::default();
    let store = MemoryStateStore::default();
    Installer::new(&backend, &store)
        .run(request())
        .expect("first install");
    let actions = backend.actions();
    let events = backend.events();
    backend.state.borrow_mut().verifications.clear();

    let outcome = Installer::new(&backend, &store)
        .run(request())
        .expect("completed rerun");

    assert_eq!(outcome, InstallOutcome::AlreadyComplete);
    assert_eq!(backend.actions(), actions);
    assert_eq!(backend.events(), events);
    assert_eq!(
        backend.state.borrow().verifications,
        InstallPhase::ALL.to_vec()
    );
}

#[test]
fn target_install_json_is_strict_complete_and_contains_no_local_data() {
    let json = br#"{
        "schema_version": 1,
        "files_changed": true,
        "boot_config_changed": false,
        "reboot_required": false,
        "revision": "0123456789abcdef0123456789abcdef01234567",
        "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
    }"#;

    let parsed = TargetInstallResult::from_json(json).expect("valid result");

    assert_eq!(parsed.schema_version, 1);
    assert!(parsed.files_changed);
    assert!(!parsed.boot_config_changed);
    assert!(!parsed.reboot_required);
    assert_eq!(parsed.revision, "0123456789abcdef0123456789abcdef01234567");
    assert_eq!(
        parsed.sha256,
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
    );
    let encoded = parsed.to_json().expect("serialize");
    assert!(!encoded.contains("/Users/"));
    assert!(!encoded.contains("password"));
    assert!(!encoded.contains("token"));
}

#[test]
fn target_install_json_rejects_hostile_partial_and_ambiguous_output() {
    let valid = r#"{"schema_version":1,"files_changed":true,"boot_config_changed":false,"reboot_required":false,"revision":"1111111111111111111111111111111111111111","sha256":"2222222222222222222222222222222222222222222222222222222222222222"}"#;
    let cases = [
        "",
        "{}",
        r#"{"schema_version":2,"files_changed":true,"boot_config_changed":false,"reboot_required":false,"revision":"1111111111111111111111111111111111111111","sha256":"2222222222222222222222222222222222222222222222222222222222222222"}"#,
        r#"{"schema_version":1,"files_changed":true,"boot_config_changed":false,"reboot_required":false,"revision":"short","sha256":"2222222222222222222222222222222222222222222222222222222222222222"}"#,
        r#"{"schema_version":1,"files_changed":true,"boot_config_changed":false,"reboot_required":false,"revision":"1111111111111111111111111111111111111111","sha256":"UPPER22222222222222222222222222222222222222222222222222222222222"}"#,
        r#"{"schema_version":1,"files_changed":true,"boot_config_changed":false,"reboot_required":false,"revision":"1111111111111111111111111111111111111111","sha256":"2222222222222222222222222222222222222222222222222222222222222222","path":"/Users/shayne/secret"}"#,
        r#"{"schema_version":1,"files_changed":true,"boot_config_changed":false,"reboot_required":false,"revision":"1111111111111111111111111111111111111111","sha256":"2222222222222222222222222222222222222222222222222222222222222222","owned_files":[]}"#,
        r#"{"schema_version":1,"schema_version":1,"files_changed":true,"boot_config_changed":false,"reboot_required":false,"revision":"1111111111111111111111111111111111111111","sha256":"2222222222222222222222222222222222222222222222222222222222222222"}"#,
        &format!("{valid}\n{{}}"),
        &format!("diagnostic\n{valid}"),
        &format!("{valid}\npartial"),
    ];

    for hostile in cases {
        assert!(
            TargetInstallResult::from_json(hostile.as_bytes()).is_err(),
            "{hostile:?}"
        );
    }
}

fn ownership_json(paths: &[&str]) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "owned_files": paths
            .iter()
            .enumerate()
            .map(|(index, path)| serde_json::json!({
                "target_path": path,
                "sha256": format!("{:x}", index + 6).repeat(64),
            }))
            .collect::<Vec<_>>(),
    }))
    .expect("ownership test JSON")
}

#[test]
fn target_install_ownership_accepts_both_exact_production_shapes() {
    let mandatory = [
        "/opt/planeradar/bin/planeradar",
        "/opt/planeradar/REVISION",
        "/opt/planeradar/SHA256",
        "/etc/systemd/system/planeradar.service",
    ];
    let with_owned_settings = [
        "/opt/planeradar/bin/planeradar",
        "/opt/planeradar/REVISION",
        "/opt/planeradar/SHA256",
        "/etc/systemd/system/planeradar.service",
        "/var/lib/planeradar/settings.json",
        "/var/lib/planeradar-installer/settings-owned-v1",
    ];

    for expected in [&mandatory[..], &with_owned_settings[..]] {
        let parsed =
            TargetInstallOwnership::from_json(&ownership_json(expected)).expect("valid ownership");
        assert_eq!(
            parsed
                .owned_files
                .iter()
                .map(|file| file.target_path.as_str())
                .collect::<Vec<_>>(),
            expected
        );
    }
}

#[test]
fn target_install_ownership_rejects_partial_reordered_duplicate_and_extra_paths() {
    let invalid = [
        (
            "settings without marker",
            vec![
                "/opt/planeradar/bin/planeradar",
                "/opt/planeradar/REVISION",
                "/opt/planeradar/SHA256",
                "/etc/systemd/system/planeradar.service",
                "/var/lib/planeradar/settings.json",
            ],
        ),
        (
            "marker without settings",
            vec![
                "/opt/planeradar/bin/planeradar",
                "/opt/planeradar/REVISION",
                "/opt/planeradar/SHA256",
                "/etc/systemd/system/planeradar.service",
                "/var/lib/planeradar-installer/settings-owned-v1",
            ],
        ),
        (
            "reordered mandatory paths",
            vec![
                "/opt/planeradar/REVISION",
                "/opt/planeradar/bin/planeradar",
                "/opt/planeradar/SHA256",
                "/etc/systemd/system/planeradar.service",
            ],
        ),
        (
            "reordered settings paths",
            vec![
                "/opt/planeradar/bin/planeradar",
                "/opt/planeradar/REVISION",
                "/opt/planeradar/SHA256",
                "/etc/systemd/system/planeradar.service",
                "/var/lib/planeradar-installer/settings-owned-v1",
                "/var/lib/planeradar/settings.json",
            ],
        ),
        (
            "duplicate path",
            vec![
                "/opt/planeradar/bin/planeradar",
                "/opt/planeradar/REVISION",
                "/opt/planeradar/SHA256",
                "/opt/planeradar/bin/planeradar",
            ],
        ),
        (
            "extra path",
            vec![
                "/opt/planeradar/bin/planeradar",
                "/opt/planeradar/REVISION",
                "/opt/planeradar/SHA256",
                "/etc/systemd/system/planeradar.service",
                "/tmp/extra",
            ],
        ),
    ];

    for (case, paths) in invalid {
        assert!(
            TargetInstallOwnership::from_json(&ownership_json(&paths)).is_err(),
            "accepted {case}"
        );
    }
}

#[test]
fn target_state_owned_files_survive_phase_persistence() {
    let backend = ScriptedBackend::default();
    let store = MemoryStateStore::default();
    let owned = OwnedFile {
        target_path: "/opt/planeradar/bin/planeradar".into(),
        sha256: "6".repeat(64),
    };
    let mut target_state = expected_target_state(InstallPhase::ApplicationInstalled);
    target_state.owned_files = vec![owned.clone()];
    store
        .save(&expected_state(InstallPhase::ApplicationInstalled))
        .expect("seed Mac state");
    backend
        .save_target_state(&target_state)
        .expect("seed target state");

    Installer::new(&backend, &store)
        .run(request())
        .expect("resume");

    assert_eq!(
        backend.target_state().expect("target state").owned_files,
        vec![owned]
    );
}

#[test]
fn first_install_persists_the_exact_owned_files_returned_by_the_target() {
    let backend = ScriptedBackend::default();
    let store = MemoryStateStore::default();

    Installer::new(&backend, &store)
        .run(request())
        .expect("fresh install");

    assert_eq!(
        backend.target_state().expect("target state").owned_files,
        expected_owned_files(&request().application)
    );
}

type ApplicationArchiveMember<'a> = (&'a str, &'a [u8], u32, u64, u64, u64, tar::EntryType);

fn application_archive(members: &[ApplicationArchiveMember<'_>]) -> Vec<u8> {
    let mut tar_bytes = Vec::new();
    {
        let encoder = zstd::stream::write::Encoder::new(&mut tar_bytes, 3).expect("zstd encoder");
        let mut archive = tar::Builder::new(encoder);
        for (name, contents, mode, uid, gid, mtime, entry_type) in members {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(*mode);
            header.set_uid(*uid);
            header.set_gid(*gid);
            header.set_mtime(*mtime);
            header.set_entry_type(*entry_type);
            header.set_cksum();
            archive
                .append_data(&mut header, name, Cursor::new(*contents))
                .expect("archive member");
        }
        archive
            .into_inner()
            .expect("finish tar")
            .finish()
            .expect("finish zstd");
    }
    tar_bytes
}

fn application_archive_with_raw_name(raw_name: &[u8]) -> Vec<u8> {
    let mut archive = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(7);
    header.set_mode(0o755);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_cksum();
    archive
        .append_data(&mut header, "planeradar", Cursor::new(b"payload"))
        .expect("archive member");
    let mut tar_bytes = archive.into_inner().expect("finish archive");
    tar_bytes[..100].fill(0);
    tar_bytes[..raw_name.len()].copy_from_slice(raw_name);
    tar_bytes[148..156].fill(b' ');
    let checksum = tar_bytes[..512]
        .iter()
        .map(|byte| u32::from(*byte))
        .sum::<u32>();
    tar_bytes[148..156].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());
    zstd::stream::encode_all(Cursor::new(tar_bytes), 3).expect("compress raw archive")
}

fn archive_digest(path: &std::path::Path) -> String {
    format!(
        "{:x}",
        Sha256::digest(fs::read(path).expect("read archive for test digest"))
    )
}

#[test]
fn application_archive_extracts_only_the_normalized_root_executable() {
    let temporary = tempfile::tempdir().expect("temporary");
    let archive = temporary.path().join("application.tar.zst");
    fs::write(
        &archive,
        application_archive(&[(
            "planeradar",
            b"verified application payload",
            0o755,
            0,
            0,
            0,
            tar::EntryType::Regular,
        )]),
    )
    .expect("write archive");

    let payload = extract_application_payload(
        &archive,
        &archive_digest(&archive),
        &temporary.path().join("cache"),
    )
    .expect("extract payload");

    assert_eq!(
        fs::read(payload.path()).expect("payload bytes"),
        b"verified application payload"
    );
    assert_eq!(payload.size(), 28);
    assert_eq!(
        payload.sha256(),
        "35b161b20e4468d72a335c20fd30f3ddc265b5689f7a6641893271d4770c5ace"
    );
    assert_eq!(
        fs::metadata(payload.path())
            .expect("payload metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[test]
fn application_archive_rejects_hostile_members_and_metadata() {
    let temporary = tempfile::tempdir().expect("temporary");
    let cases = [
        application_archive(&[]),
        application_archive(&[
            (
                "planeradar",
                b"one",
                0o755,
                0,
                0,
                0,
                tar::EntryType::Regular,
            ),
            ("extra", b"two", 0o755, 0, 0, 0, tar::EntryType::Regular),
        ]),
        application_archive(&[(
            "nested/planeradar",
            b"payload",
            0o755,
            0,
            0,
            0,
            tar::EntryType::Regular,
        )]),
        application_archive(&[(
            "planeradar",
            b"payload",
            0o644,
            0,
            0,
            0,
            tar::EntryType::Regular,
        )]),
        application_archive(&[(
            "planeradar",
            b"payload",
            0o755,
            1000,
            0,
            0,
            tar::EntryType::Regular,
        )]),
        application_archive(&[(
            "planeradar",
            b"payload",
            0o755,
            0,
            0,
            1,
            tar::EntryType::Regular,
        )]),
        application_archive(&[(
            "planeradar",
            b"payload",
            0o755,
            0,
            0,
            0,
            tar::EntryType::Symlink,
        )]),
        application_archive_with_raw_name(b"../planeradar"),
        application_archive_with_raw_name(b"/planeradar"),
    ];

    for (index, bytes) in cases.into_iter().enumerate() {
        let archive = temporary.path().join(format!("hostile-{index}.tar.zst"));
        fs::write(&archive, bytes).expect("write hostile archive");
        assert!(
            extract_application_payload(
                &archive,
                &archive_digest(&archive),
                &temporary.path().join("cache")
            )
            .is_err(),
            "hostile case {index} was accepted"
        );
    }
}

#[test]
fn application_archive_rejects_symlink_inputs_and_concatenated_archives() {
    let temporary = tempfile::tempdir().expect("temporary");
    let archive = temporary.path().join("application.tar.zst");
    let bytes = application_archive(&[(
        "planeradar",
        b"payload",
        0o755,
        0,
        0,
        0,
        tar::EntryType::Regular,
    )]);
    fs::write(&archive, &bytes).expect("write archive");

    let linked = temporary.path().join("linked.tar.zst");
    std::os::unix::fs::symlink(&archive, &linked).expect("archive symlink");
    assert!(
        extract_application_payload(
            &linked,
            &archive_digest(&archive),
            &temporary.path().join("cache")
        )
        .is_err()
    );

    let concatenated = temporary.path().join("concatenated.tar.zst");
    fs::write(&concatenated, [bytes.as_slice(), bytes.as_slice()].concat())
        .expect("concatenated archive");
    assert!(
        extract_application_payload(
            &concatenated,
            &archive_digest(&concatenated),
            &temporary.path().join("cache")
        )
        .is_err()
    );
}

#[test]
fn application_archive_is_bound_to_the_manifest_digest_and_total_expansion() {
    let temporary = tempfile::tempdir().expect("temporary");
    let archive = temporary.path().join("application.tar.zst");
    fs::write(
        &archive,
        application_archive(&[(
            "planeradar",
            b"payload",
            0o755,
            0,
            0,
            0,
            tar::EntryType::Regular,
        )]),
    )
    .expect("write archive");

    assert!(
        extract_application_payload(
            &archive,
            &"0".repeat(64),
            &temporary.path().join("digest-cache")
        )
        .is_err()
    );

    let expansion = temporary.path().join("expansion.tar.zst");
    let oversized = vec![0_u8; 66 * 1024 * 1024 + 1];
    fs::write(
        &expansion,
        zstd::stream::encode_all(Cursor::new(oversized), 3).expect("compress expansion"),
    )
    .expect("write expansion");
    assert!(
        extract_application_payload(
            &expansion,
            &archive_digest(&expansion),
            &temporary.path().join("expansion-cache")
        )
        .is_err()
    );
}
