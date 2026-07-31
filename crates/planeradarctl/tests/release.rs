use std::{
    collections::HashMap,
    fs,
    io::Write,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use jsonschema::Draft;
use planeradarctl::{
    DriverLock,
    release::{
        APP_REPOSITORY, Architecture, Artifact, ArtifactKind, DownloadRequest,
        GhLatestStableReleaseResolver, GhReleaseSource, Platform, ReleaseClient, ReleaseError,
        ReleaseInput, ReleaseManifest, ReleaseSource, ReleaseSourceError, ResolvedArtifact,
        ResolvedRelease, StreamingCommandRunner, SupportedTarget, SystemStreamingCommandRunner,
        Verifier,
    },
    transport::{CommandOutput, CommandRunner, Invocation, RunnerError},
};
use semver::Version;
use serde_json::{Value, json};

const VALID_MANIFEST: &str = include_str!("../../../tests/fixtures/releases/valid.json");
const APP_BYTES: &[u8] = b"planeradar-binary";
const CONTROL_ARM64_BYTES: &[u8] = b"control-arm64-bin";
const CONTROL_X86_64_BYTES: &[u8] = b"control-x86_64-bin";
const APP_NAME: &str = "planeradar-aarch64-linux-gnu.tar.zst";

fn lock() -> DriverLock {
    DriverLock::checked_in().expect("checked-in driver lock")
}

fn requested(version: &str) -> Version {
    Version::parse(version).expect("test version")
}

struct ReleaseViewRunner {
    output: Mutex<Option<Result<CommandOutput, RunnerError>>>,
    invocations: Mutex<Vec<Invocation>>,
}

impl ReleaseViewRunner {
    fn output(status: i32, stdout: impl Into<Vec<u8>>) -> Self {
        Self {
            output: Mutex::new(Some(Ok(CommandOutput::new(
                status,
                stdout.into(),
                Vec::new(),
            )))),
            invocations: Mutex::new(Vec::new()),
        }
    }

    fn runner_error() -> Self {
        Self {
            output: Mutex::new(Some(Err(RunnerError::Failed))),
            invocations: Mutex::new(Vec::new()),
        }
    }
}

impl CommandRunner for &ReleaseViewRunner {
    fn run(&self, invocation: Invocation) -> Result<CommandOutput, RunnerError> {
        self.invocations
            .lock()
            .expect("release view invocations")
            .push(invocation);
        self.output
            .lock()
            .expect("release view output")
            .take()
            .expect("one release view invocation")
    }
}

#[test]
fn latest_stable_resolver_uses_one_exact_bounded_release_view() {
    let runner = ReleaseViewRunner::output(
        0,
        br#"{"tagName":"v1.2.3","isDraft":false,"isPrerelease":false}"#.as_slice(),
    );

    let resolved = GhLatestStableReleaseResolver::new(&runner)
        .resolve()
        .expect("latest stable release");

    assert_eq!(resolved, requested("1.2.3"));
    let invocations = runner.invocations.lock().expect("release view invocations");
    let [invocation] = invocations.as_slice() else {
        panic!("expected one release view invocation");
    };
    assert_eq!(invocation.program(), "gh");
    assert_eq!(
        invocation.arguments(),
        [
            "release",
            "view",
            "-R",
            "shayne/RPi-Plane-Radar",
            "--json",
            "tagName,isDraft,isPrerelease",
        ]
    );
    assert_eq!(
        invocation.timeout(),
        Some(std::time::Duration::from_secs(15))
    );
    assert_eq!(invocation.stdout_limit(), Some(1024));
}

#[test]
fn latest_stable_resolver_rejects_nonstable_noncanonical_and_unbounded_output() {
    let oversized = vec![b'x'; 1025];
    let cases = [
        (
            "draft",
            ReleaseViewRunner::output(
                0,
                br#"{"tagName":"v1.2.3","isDraft":true,"isPrerelease":false}"#.as_slice(),
            ),
        ),
        (
            "prerelease",
            ReleaseViewRunner::output(
                0,
                br#"{"tagName":"v1.2.3-rc.1","isDraft":false,"isPrerelease":true}"#.as_slice(),
            ),
        ),
        (
            "malformed tag",
            ReleaseViewRunner::output(
                0,
                br#"{"tagName":"1.2.3","isDraft":false,"isPrerelease":false}"#.as_slice(),
            ),
        ),
        (
            "noncanonical tag",
            ReleaseViewRunner::output(
                0,
                br#"{"tagName":"v01.2.3","isDraft":false,"isPrerelease":false}"#.as_slice(),
            ),
        ),
        (
            "trailing JSON",
            ReleaseViewRunner::output(
                0,
                br#"{"tagName":"v1.2.3","isDraft":false,"isPrerelease":false}{}"#.as_slice(),
            ),
        ),
        ("oversized output", ReleaseViewRunner::output(0, oversized)),
        (
            "nonzero command",
            ReleaseViewRunner::output(
                1,
                br#"{"tagName":"v1.2.3","isDraft":false,"isPrerelease":false}"#.as_slice(),
            ),
        ),
        ("runner error", ReleaseViewRunner::runner_error()),
    ];

    for (name, runner) in cases {
        assert!(
            GhLatestStableReleaseResolver::new(&runner)
                .resolve()
                .is_err(),
            "{name} must be rejected"
        );
    }
}

fn valid_value() -> Value {
    serde_json::from_str(VALID_MANIFEST).expect("valid fixture JSON")
}

fn parse_value(value: &Value, version: &str) -> Result<ReleaseManifest, ReleaseError> {
    ReleaseManifest::parse(
        &serde_json::to_vec(value).expect("serialize test manifest"),
        &requested(version),
        &lock(),
    )
}

fn set_path(value: &mut Value, pointer: &str, replacement: Value) {
    *value.pointer_mut(pointer).expect("fixture pointer") = replacement;
}

fn set_version(value: &mut Value, version: &str) {
    set_path(value, "/version", json!(version));
}

fn rename_artifact(value: &mut Value, from: &str, to: &str) {
    let artifacts = value["artifacts"].as_object_mut().expect("artifact object");
    let metadata = artifacts.remove(from).expect("artifact key");
    artifacts.insert(to.into(), metadata);
}

#[test]
fn parses_the_complete_valid_manifest_and_reuses_the_checked_in_driver_lock() {
    let manifest =
        ReleaseManifest::parse(VALID_MANIFEST.as_bytes(), &requested("0.1.0-rc.1"), &lock())
            .expect("valid manifest");

    assert_eq!(manifest.schema_version, 1);
    assert_eq!(manifest.version, requested("0.1.0-rc.1"));
    assert_eq!(
        manifest.source_commit,
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    );
    assert_eq!(
        manifest.supported,
        SupportedTarget {
            model: "Raspberry Pi Zero 2 W".into(),
            display: "HyperPixel 2.1 Round".into(),
            operating_system: "Raspberry Pi OS Lite Trixie (64-bit)".into(),
            architecture: Architecture::Aarch64,
            kernel_policy: "driver-manifest-supported".into(),
        }
    );
    assert_eq!(manifest.driver.repository, lock().repository);
    assert_eq!(manifest.driver.version, lock().version);
    assert_eq!(manifest.driver.commit, lock().commit);
    assert_eq!(manifest.driver.manifest_sha256, lock().manifest_sha256);
    assert_eq!(manifest.artifacts.len(), 3);
    assert_eq!(
        manifest.artifacts[0],
        Artifact {
            name: "planeradar-aarch64-linux-gnu.tar.zst".into(),
            kind: ArtifactKind::Application,
            platform: Platform::LinuxGnu,
            architecture: Architecture::Aarch64,
            size: 17,
            sha256: "a8b3f6f4320547c3ef85f3860638f2f0156459307aa4d9e7c369cb8917ace9da".into(),
            runnable: true,
        }
    );
}

#[test]
fn focused_invalid_fixtures_are_rejected() {
    for fixture in [
        include_str!("../../../tests/fixtures/releases/invalid-schema-version.json"),
        include_str!("../../../tests/fixtures/releases/invalid-unknown-field.json"),
        include_str!("../../../tests/fixtures/releases/invalid-traversal.json"),
        include_str!("../../../tests/fixtures/releases/invalid-digest.json"),
    ] {
        assert!(
            ReleaseManifest::parse(fixture.as_bytes(), &requested("0.1.0-rc.1"), &lock()).is_err()
        );
    }
}

#[test]
fn rejects_unknown_fields_at_every_manifest_object_level() {
    for pointer in [
        "",
        "/supported",
        "/driver",
        "/artifacts/planeradar-aarch64-linux-gnu.tar.zst",
    ] {
        let mut value = valid_value();
        value
            .pointer_mut(pointer)
            .expect("object")
            .as_object_mut()
            .expect("JSON object")
            .insert("unexpected".into(), json!("not allowed"));
        assert!(
            parse_value(&value, "0.1.0-rc.1").is_err(),
            "unknown field at {pointer:?} was accepted"
        );
    }
}

#[test]
fn rejects_schema_version_empty_and_oversized_artifact_objects() {
    let mut wrong_schema = valid_value();
    set_path(&mut wrong_schema, "/schema_version", json!(2));
    assert!(parse_value(&wrong_schema, "0.1.0-rc.1").is_err());

    let mut empty = valid_value();
    set_path(&mut empty, "/artifacts", json!({}));
    assert!(parse_value(&empty, "0.1.0-rc.1").is_err());

    let mut too_many = valid_value();
    let exemplar = too_many["artifacts"][APP_NAME].clone();
    too_many["artifacts"] = Value::Object(
        (0..65)
            .map(|index| (format!("artifact-{index}"), exemplar.clone()))
            .collect(),
    );
    assert!(parse_value(&too_many, "0.1.0-rc.1").is_err());
}

#[test]
fn rejects_duplicate_artifact_object_keys_even_when_metadata_differs() {
    let duplicate = VALID_MANIFEST.replacen(
        r#""artifacts": {"#,
        r#""artifacts": {
    "planeradar-aarch64-linux-gnu.tar.zst": {
      "architecture": "any",
      "size": 1,
      "sha256": "1111111111111111111111111111111111111111111111111111111111111111",
      "runnable": false
    },"#,
        1,
    );
    assert!(
        ReleaseManifest::parse(duplicate.as_bytes(), &requested("0.1.0-rc.1"), &lock()).is_err()
    );
}

#[test]
fn rejects_every_driver_lock_mismatch() {
    for (pointer, replacement) in [
        (
            "/driver/repository",
            json!("https://github.com/attacker/hyperpixel2r-kms"),
        ),
        (
            "/driver/commit",
            json!("1111111111111111111111111111111111111111"),
        ),
        ("/driver/version", json!("0.1.0-rc.3")),
        (
            "/driver/manifest_sha256",
            json!("1111111111111111111111111111111111111111111111111111111111111111"),
        ),
    ] {
        let mut value = valid_value();
        set_path(&mut value, pointer, replacement);
        assert!(
            parse_value(&value, "0.1.0-rc.1").is_err(),
            "mismatch at {pointer} was accepted"
        );
    }
}

#[test]
fn rejects_forged_supplied_driver_lock_identity_and_grammar() {
    for (forged, pointer, raw_value) in [
        (
            DriverLock {
                repository: "https://github.com/attacker/hyperpixel2r-kms".into(),
                ..lock()
            },
            "/driver/repository",
            json!("https://github.com/attacker/hyperpixel2r-kms"),
        ),
        (
            DriverLock {
                commit: "not-a-lowercase-full-commit".into(),
                ..lock()
            },
            "/driver/commit",
            json!("not-a-lowercase-full-commit"),
        ),
        (
            DriverLock {
                manifest_sha256: "BAD-DIGEST".into(),
                ..lock()
            },
            "/driver/manifest_sha256",
            json!("BAD-DIGEST"),
        ),
    ] {
        let mut matching_forged_manifest = valid_value();
        set_path(&mut matching_forged_manifest, pointer, raw_value);
        assert!(
            ReleaseManifest::parse(
                &serde_json::to_vec(&matching_forged_manifest).expect("forged manifest"),
                &requested("0.1.0-rc.1"),
                &forged
            )
            .is_err()
        );
    }
}

#[test]
fn rejects_requested_version_mismatch_and_noncanonical_semver() {
    assert!(parse_value(&valid_value(), "0.1.0-rc.2").is_err());

    for (invalid, requested_version) in [
        ("v0.1.0", "0.1.0"),
        ("0.1", "0.1.0"),
        ("01.1.0", "1.1.0"),
        ("0.1.0-01", "0.1.0"),
    ] {
        let mut value = valid_value();
        set_path(&mut value, "/version", json!(invalid));
        assert!(
            parse_value(&value, requested_version).is_err(),
            "version {invalid:?} was accepted"
        );
    }
}

#[test]
fn rejects_invalid_source_commit_grammar() {
    for invalid in [
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "gggggggggggggggggggggggggggggggggggggggg",
    ] {
        let mut value = valid_value();
        set_path(&mut value, "/source_commit", json!(invalid));
        assert!(parse_value(&value, "0.1.0-rc.1").is_err());
    }
}

#[test]
fn rejects_workflow_refs_that_cannot_be_the_dispatch_oidc_ref() {
    for invalid in [
        "main",
        "refs/pull/1/merge",
        "refs/heads/",
        "refs/heads/feature..branch",
        "refs/heads/feature/",
        "refs/tags/release.lock",
    ] {
        let mut value = valid_value();
        set_path(&mut value, "/workflow/ref", json!(invalid));
        assert!(
            parse_value(&value, "0.1.0-rc.1").is_err(),
            "unsafe workflow ref {invalid:?} was accepted"
        );
    }
}

#[test]
fn release_kind_requires_its_exact_signing_workflow() {
    let candidate = valid_value();
    assert!(
        parse_value(&candidate, "0.1.0-rc.1").is_ok(),
        "release candidates must accept the candidate workflow"
    );

    let mut candidate_with_stable_workflow = candidate.clone();
    set_path(
        &mut candidate_with_stable_workflow,
        "/workflow/path",
        json!(".github/workflows/stable-draft.yml"),
    );
    assert!(
        parse_value(&candidate_with_stable_workflow, "0.1.0-rc.1").is_err(),
        "release candidates must reject the stable signer workflow"
    );

    let mut stable = candidate;
    set_version(&mut stable, "0.1.0");
    set_path(
        &mut stable,
        "/workflow/path",
        json!(".github/workflows/stable-draft.yml"),
    );
    assert!(
        parse_value(&stable, "0.1.0").is_ok(),
        "stable releases must accept only the stable draft workflow"
    );

    set_path(
        &mut stable,
        "/workflow/path",
        json!(".github/workflows/release.yml"),
    );
    assert!(
        parse_value(&stable, "0.1.0").is_err(),
        "stable releases must reject the candidate signer workflow"
    );
}

#[test]
fn rejects_wrong_supported_target_and_artifact_architecture() {
    for (pointer, replacement) in [
        ("/supported/model", json!("Raspberry Pi 4 Model B")),
        (
            "/supported/operating_system",
            json!("Raspberry Pi OS Lite Bookworm (64-bit)"),
        ),
        ("/supported/architecture", json!("armv7")),
        (
            "/artifacts/planeradar-aarch64-linux-gnu.tar.zst/architecture",
            json!("armv7"),
        ),
    ] {
        let mut value = valid_value();
        set_path(&mut value, pointer, replacement);
        assert!(
            parse_value(&value, "0.1.0-rc.1").is_err(),
            "wrong target at {pointer} was accepted"
        );
    }
}

#[test]
fn accepts_closed_cross_platform_artifact_architectures_but_requires_the_pi_app() {
    let value = valid_value();
    let parsed = parse_value(&value, "0.1.0-rc.1").expect("closed architecture values");
    assert_eq!(
        parsed
            .artifacts
            .iter()
            .find(|artifact| artifact.name == "planeradarctl-aarch64-apple-darwin.tar.zst")
            .expect("Apple Silicon control artifact")
            .architecture,
        Architecture::Aarch64
    );
    assert_eq!(
        parsed
            .artifacts
            .iter()
            .find(|artifact| artifact.name == "planeradarctl-x86_64-apple-darwin.tar.zst")
            .expect("Mac control artifact")
            .architecture,
        Architecture::X86_64
    );

    let mut renamed = valid_value();
    rename_artifact(&mut renamed, APP_NAME, "documentation.json");
    assert!(parse_value(&renamed, "0.1.0-rc.1").is_err());

    for (pointer, replacement) in [
        (
            "/artifacts/planeradar-aarch64-linux-gnu.tar.zst/architecture",
            json!("any"),
        ),
        (
            "/artifacts/planeradar-aarch64-linux-gnu.tar.zst/runnable",
            json!(false),
        ),
        (
            "/artifacts/planeradarctl-x86_64-apple-darwin.tar.zst/platform",
            json!("linux-gnu"),
        ),
    ] {
        let mut invalid = valid_value();
        set_path(&mut invalid, pointer, replacement);
        assert!(
            parse_value(&invalid, "0.1.0-rc.1").is_err(),
            "required app invariant at {pointer} was not enforced"
        );
    }
}

#[test]
fn rejects_unsafe_artifact_names() {
    for invalid in [
        "",
        ".",
        "..",
        "/absolute",
        "nested/file",
        r"nested\file",
        "../escape",
        "a..",
        "-output",
        "white space",
        "glob*",
        "dollar$",
        "colon:name",
        "control\u{0007}",
        &"a".repeat(129),
    ] {
        let mut value = valid_value();
        rename_artifact(&mut value, APP_NAME, invalid);
        assert!(
            parse_value(&value, "0.1.0-rc.1").is_err(),
            "unsafe name {invalid:?} was accepted"
        );
    }
}

#[test]
fn rejects_bad_digests_and_unbounded_sizes() {
    for digest in [
        "a8b3",
        "A8B3F6F4320547C3EF85F3860638F2F0156459307AA4D9E7C369CB8917ACE9DA",
        "g8b3f6f4320547c3ef85f3860638f2f0156459307aa4d9e7c369cb8917ace9da",
    ] {
        let mut value = valid_value();
        set_path(
            &mut value,
            "/artifacts/planeradar-aarch64-linux-gnu.tar.zst/sha256",
            json!(digest),
        );
        assert!(parse_value(&value, "0.1.0-rc.1").is_err());
    }
    for size in [0_u64, 134_217_729] {
        let mut value = valid_value();
        set_path(
            &mut value,
            "/artifacts/planeradar-aarch64-linux-gnu.tar.zst/size",
            json!(size),
        );
        assert!(parse_value(&value, "0.1.0-rc.1").is_err());
    }

    let mut oversized_control = valid_value();
    set_path(
        &mut oversized_control,
        "/artifacts/planeradarctl-aarch64-apple-darwin.tar.zst/size",
        json!(16 * 1024 * 1024 + 1),
    );
    assert!(
        parse_value(&oversized_control, "0.1.0-rc.1").is_err(),
        "runtime parser must enforce the schema's narrow control-archive cap"
    );
}

#[test]
fn rejects_trailing_json_and_duplicate_keys_recursively() {
    let trailing = format!("{VALID_MANIFEST}{{}}");
    assert!(
        ReleaseManifest::parse(trailing.as_bytes(), &requested("0.1.0-rc.1"), &lock()).is_err()
    );

    let duplicate_top = VALID_MANIFEST.replacen(
        r#""schema_version": 1,"#,
        r#""schema_version": 1, "schema_version": 1,"#,
        1,
    );
    assert!(
        ReleaseManifest::parse(duplicate_top.as_bytes(), &requested("0.1.0-rc.1"), &lock())
            .is_err()
    );

    let duplicate_nested = VALID_MANIFEST.replacen(
        r#""model": "Raspberry Pi Zero 2 W","#,
        r#""model": "Raspberry Pi Zero 2 W", "model": "Raspberry Pi Zero 2 W","#,
        1,
    );
    assert!(
        ReleaseManifest::parse(
            duplicate_nested.as_bytes(),
            &requested("0.1.0-rc.1"),
            &lock()
        )
        .is_err()
    );
}

#[test]
fn schema_declares_the_same_closed_runtime_boundaries() {
    let schema: Value = serde_json::from_str(include_str!(
        "../../../release/release-manifest.schema.json"
    ))
    .expect("schema JSON");

    assert_eq!(
        schema["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["properties"]["schema_version"]["const"], 1);
    assert_eq!(
        schema["properties"]["supported"]["properties"]["model"]["const"],
        "Raspberry Pi Zero 2 W"
    );
    assert_eq!(
        schema["properties"]["supported"]["properties"]["architecture"]["const"],
        "aarch64"
    );
    assert_eq!(
        schema["properties"]["driver"]["properties"]["repository"]["const"],
        "https://github.com/shayne/hyperpixel2r-kms"
    );
    assert_eq!(schema["$defs"]["commit"]["pattern"], "^[0-9a-f]{40}$");
    assert_eq!(schema["$defs"]["sha256"]["pattern"], "^[0-9a-f]{64}$");
    assert_eq!(schema["$defs"]["artifact"]["additionalProperties"], false);
    assert_eq!(
        schema["$defs"]["artifact"]["properties"]["size"]["minimum"],
        1
    );
    assert_eq!(
        schema["$defs"]["artifact"]["properties"]["size"]["maximum"],
        134_217_728_u64
    );
    assert_eq!(schema["properties"]["artifacts"]["maxProperties"], 3);
    assert_eq!(
        schema["$defs"]["artifact"]["properties"]["architecture"]["enum"],
        json!(["aarch64", "x86_64"])
    );
    assert_eq!(
        schema["properties"]["artifacts"]["required"],
        json!([
            "planeradar-aarch64-linux-gnu.tar.zst",
            "planeradarctl-aarch64-apple-darwin.tar.zst",
            "planeradarctl-x86_64-apple-darwin.tar.zst"
        ])
    );
    assert_eq!(
        schema["properties"]["artifacts"]["properties"]["planeradar-aarch64-linux-gnu.tar.zst"]["allOf"]
            [1]["properties"]["runnable"]["const"],
        true
    );
}

#[test]
fn draft_2020_12_schema_compiles_and_enforces_the_runtime_name_contract() {
    let schema: Value = serde_json::from_str(include_str!(
        "../../../release/release-manifest.schema.json"
    ))
    .expect("schema JSON");
    let validator = jsonschema::options()
        .with_draft(Draft::Draft202012)
        .build(&schema)
        .expect("compile Draft 2020-12 schema");
    let valid = valid_value();
    assert!(validator.is_valid(&valid));

    let mut legacy_array = valid.clone();
    set_path(&mut legacy_array, "/artifacts", json!([]));
    assert!(!validator.is_valid(&legacy_array));

    let mut consecutive_dots = valid.clone();
    rename_artifact(&mut consecutive_dots, APP_NAME, "a..");
    assert!(!validator.is_valid(&consecutive_dots));
    assert!(parse_value(&consecutive_dots, "0.1.0-rc.1").is_err());

    let mut unknown_metadata = valid.clone();
    unknown_metadata["artifacts"][APP_NAME]
        .as_object_mut()
        .expect("artifact metadata")
        .insert("name".into(), json!(APP_NAME));
    assert!(!validator.is_valid(&unknown_metadata));

    for fixture in [
        include_str!("../../../tests/fixtures/releases/invalid-schema-version.json"),
        include_str!("../../../tests/fixtures/releases/invalid-unknown-field.json"),
        include_str!("../../../tests/fixtures/releases/invalid-traversal.json"),
        include_str!("../../../tests/fixtures/releases/invalid-digest.json"),
    ] {
        let instance: Value = serde_json::from_str(fixture).expect("fixture JSON");
        assert!(!validator.is_valid(&instance));
    }
}

#[test]
fn schema_v1_allows_future_grammatical_driver_locks_while_runtime_pins_this_build() {
    let schema: Value = serde_json::from_str(include_str!(
        "../../../release/release-manifest.schema.json"
    ))
    .expect("schema JSON");
    let validator = jsonschema::options()
        .with_draft(Draft::Draft202012)
        .build(&schema)
        .expect("compile schema");
    let mut future = valid_value();
    set_path(&mut future, "/driver/version", json!("0.2.0"));
    set_path(
        &mut future,
        "/driver/commit",
        json!("1111111111111111111111111111111111111111"),
    );
    set_path(
        &mut future,
        "/driver/manifest_sha256",
        json!("2222222222222222222222222222222222222222222222222222222222222222"),
    );
    assert!(validator.is_valid(&future));
    assert!(parse_value(&future, "0.1.0-rc.1").is_err());
}

#[test]
fn release_input_debug_redacts_direct_local_paths() {
    let secret = Path::new("/Users/maintainer/private/release");
    let rendered = format!("{:?}", ReleaseInput::Local(secret));
    assert!(!rendered.contains(&secret.to_string_lossy().into_owned()));
    assert!(rendered.contains("redacted"));
    assert_eq!(format!("{:?}", ReleaseInput::Downloaded), "Downloaded");
}

#[derive(Clone, Default)]
struct FakeSource {
    streams: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    requests: Arc<Mutex<Vec<DownloadRequest>>>,
}

impl FakeSource {
    fn with_release(manifest: Vec<u8>) -> Self {
        let source = Self::default();
        source.set("release-manifest.json", manifest);
        source.set("planeradar-aarch64-linux-gnu.tar.zst", APP_BYTES.to_vec());
        source.set(
            "planeradarctl-aarch64-apple-darwin.tar.zst",
            CONTROL_ARM64_BYTES.to_vec(),
        );
        source.set(
            "planeradarctl-x86_64-apple-darwin.tar.zst",
            CONTROL_X86_64_BYTES.to_vec(),
        );
        source
    }

    fn set(&self, name: &str, bytes: Vec<u8>) {
        self.streams
            .lock()
            .expect("fake streams")
            .insert(name.into(), bytes);
    }

    fn remove(&self, name: &str) {
        self.streams.lock().expect("fake streams").remove(name);
    }

    fn request_count(&self, name: &str) -> usize {
        self.requests
            .lock()
            .expect("fake requests")
            .iter()
            .filter(|request| request.name() == name)
            .count()
    }
}

impl ReleaseSource for FakeSource {
    fn stream(
        &self,
        request: &DownloadRequest,
        sink: &mut dyn Write,
    ) -> Result<(), ReleaseSourceError> {
        self.requests
            .lock()
            .expect("fake requests")
            .push(request.clone());
        let bytes = self
            .streams
            .lock()
            .expect("fake streams")
            .get(request.name())
            .cloned()
            .ok_or(ReleaseSourceError::Failed)?;
        for chunk in bytes.chunks(3) {
            sink.write_all(chunk)
                .map_err(|_| ReleaseSourceError::Failed)?;
        }
        Ok(())
    }
}

fn write_local_release(directory: &Path, manifest: &[u8]) {
    fs::create_dir_all(directory).expect("create local release");
    fs::write(directory.join("release-manifest.json"), manifest).expect("write manifest");
    fs::write(
        directory.join("planeradar-aarch64-linux-gnu.tar.zst"),
        APP_BYTES,
    )
    .expect("write app artifact");
    fs::write(
        directory.join("planeradarctl-aarch64-apple-darwin.tar.zst"),
        CONTROL_ARM64_BYTES,
    )
    .expect("write arm64 control artifact");
    fs::write(
        directory.join("planeradarctl-x86_64-apple-darwin.tar.zst"),
        CONTROL_X86_64_BYTES,
    )
    .expect("write x86_64 control artifact");
}

fn directory_snapshot(directory: &Path) -> Vec<(String, Vec<u8>, u32)> {
    let mut snapshot = fs::read_dir(directory)
        .expect("read local release")
        .map(|entry| {
            let entry = entry.expect("directory entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            let metadata = fs::symlink_metadata(entry.path()).expect("metadata");
            let contents = if metadata.is_file() {
                fs::read(entry.path()).expect("read source file")
            } else {
                Vec::new()
            };
            (name, contents, metadata.permissions().mode() & 0o777)
        })
        .collect::<Vec<_>>();
    snapshot.sort();
    snapshot
}

fn resolve_downloaded(source: FakeSource, cache: &Path) -> Result<ResolvedRelease, ReleaseError> {
    ReleaseClient::new(source, cache.to_owned()).resolve(
        &requested("0.1.0-rc.1"),
        &lock(),
        ReleaseInput::Downloaded,
    )
}

#[test]
fn resolves_a_valid_read_only_local_release_without_mutating_it() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let local = temporary.path().join("local");
    let cache = temporary.path().join("cache");
    write_local_release(&local, VALID_MANIFEST.as_bytes());
    for name in [
        "release-manifest.json",
        "planeradar-aarch64-linux-gnu.tar.zst",
        "planeradarctl-aarch64-apple-darwin.tar.zst",
        "planeradarctl-x86_64-apple-darwin.tar.zst",
    ] {
        fs::set_permissions(local.join(name), fs::Permissions::from_mode(0o444))
            .expect("make source read-only");
    }
    fs::set_permissions(&local, fs::Permissions::from_mode(0o555))
        .expect("make directory read-only");
    let before = directory_snapshot(&local);

    let resolved = ReleaseClient::new(FakeSource::default(), cache)
        .resolve(
            &requested("0.1.0-rc.1"),
            &lock(),
            ReleaseInput::Local(&local),
        )
        .expect("resolve local release");

    assert_eq!(resolved.manifest.version, requested("0.1.0-rc.1"));
    assert_eq!(resolved.artifacts.len(), 3);
    assert_eq!(directory_snapshot(&local), before);
    fs::set_permissions(&local, fs::Permissions::from_mode(0o700))
        .expect("restore cleanup permissions");
}

#[test]
fn rejects_a_differing_read_only_local_file_without_source_mutation() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let local = temporary.path().join("local");
    write_local_release(&local, VALID_MANIFEST.as_bytes());
    fs::write(
        local.join("planeradar-aarch64-linux-gnu.tar.zst"),
        b"planeradar-binarx",
    )
    .expect("replace artifact with same-size corruption");
    for name in [
        "release-manifest.json",
        "planeradar-aarch64-linux-gnu.tar.zst",
        "planeradarctl-aarch64-apple-darwin.tar.zst",
        "planeradarctl-x86_64-apple-darwin.tar.zst",
    ] {
        fs::set_permissions(local.join(name), fs::Permissions::from_mode(0o444))
            .expect("make source read-only");
    }
    fs::set_permissions(&local, fs::Permissions::from_mode(0o555))
        .expect("make directory read-only");
    let before = directory_snapshot(&local);

    let result = ReleaseClient::new(FakeSource::default(), temporary.path().join("cache")).resolve(
        &requested("0.1.0-rc.1"),
        &lock(),
        ReleaseInput::Local(&local),
    );

    assert!(result.is_err());
    assert_eq!(directory_snapshot(&local), before);
    fs::set_permissions(&local, fs::Permissions::from_mode(0o700))
        .expect("restore cleanup permissions");
}

#[test]
fn rejects_missing_nonregular_and_symlinked_local_assets() {
    for mode in ["missing", "directory", "symlink"] {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let local = temporary.path().join("local");
        let artifact = local.join("planeradar-aarch64-linux-gnu.tar.zst");
        write_local_release(&local, VALID_MANIFEST.as_bytes());
        fs::remove_file(&artifact).expect("remove artifact");
        match mode {
            "missing" => {}
            "directory" => fs::create_dir(&artifact).expect("artifact directory"),
            "symlink" => {
                let outside = temporary.path().join("outside");
                fs::write(&outside, APP_BYTES).expect("outside file");
                symlink(&outside, &artifact).expect("artifact symlink");
            }
            _ => unreachable!(),
        }

        let result = ReleaseClient::new(FakeSource::default(), temporary.path().join("cache"))
            .resolve(
                &requested("0.1.0-rc.1"),
                &lock(),
                ReleaseInput::Local(&local),
            );
        assert!(result.is_err(), "{mode} asset was accepted");
    }
}

#[test]
fn rejects_symlinked_manifest_and_local_release_directory() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let real = temporary.path().join("real");
    let alias = temporary.path().join("alias");
    write_local_release(&real, VALID_MANIFEST.as_bytes());
    symlink(&real, &alias).expect("release directory symlink");
    let client = ReleaseClient::new(FakeSource::default(), temporary.path().join("cache"));
    assert!(
        client
            .resolve(
                &requested("0.1.0-rc.1"),
                &lock(),
                ReleaseInput::Local(&alias)
            )
            .is_err()
    );

    let manifest = real.join("release-manifest.json");
    let outside = temporary.path().join("outside-manifest");
    fs::rename(&manifest, &outside).expect("move manifest");
    symlink(&outside, &manifest).expect("manifest symlink");
    assert!(
        client
            .resolve(
                &requested("0.1.0-rc.1"),
                &lock(),
                ReleaseInput::Local(&real)
            )
            .is_err()
    );
}

#[test]
fn downloaded_artifacts_are_streamed_to_a_private_atomic_content_cache() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let source = FakeSource::with_release(VALID_MANIFEST.as_bytes().to_vec());

    let resolved = resolve_downloaded(source.clone(), &temporary.path().join("cache"))
        .expect("resolve download");

    let runnable = resolved
        .artifacts
        .iter()
        .find(|artifact| artifact.artifact.runnable)
        .expect("runnable artifact");
    assert_eq!(fs::read(&runnable.path).expect("cached bytes"), APP_BYTES);
    assert_eq!(
        fs::metadata(&runnable.path)
            .expect("cached metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let root = temporary.path().join("cache");
    let digest = runnable.path.parent().expect("digest directory");
    for directory in [root.as_path(), root.join("artifacts").as_path(), digest] {
        assert_eq!(
            fs::metadata(directory)
                .expect("cache directory metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    assert!(
        runnable
            .path
            .to_string_lossy()
            .contains(&runnable.artifact.sha256)
    );
    assert_eq!(source.request_count("release-manifest.json"), 1);
    assert_eq!(
        source.request_count("planeradar-aarch64-linux-gnu.tar.zst"),
        1
    );
}

#[test]
fn cache_hits_are_revalidated_without_redownloading_artifacts() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let cache = temporary.path().join("cache");
    let source = FakeSource::with_release(VALID_MANIFEST.as_bytes().to_vec());
    let first = resolve_downloaded(source.clone(), &cache).expect("first resolve");
    source.remove("planeradar-aarch64-linux-gnu.tar.zst");
    source.remove("planeradarctl-aarch64-apple-darwin.tar.zst");
    source.remove("planeradarctl-x86_64-apple-darwin.tar.zst");

    let second = resolve_downloaded(source.clone(), &cache).expect("cache hit");

    assert_eq!(first.artifacts, second.artifacts);
    assert_eq!(
        source.request_count("planeradar-aarch64-linux-gnu.tar.zst"),
        1
    );
    assert_eq!(
        source.request_count("planeradarctl-aarch64-apple-darwin.tar.zst"),
        1
    );
    assert_eq!(
        source.request_count("planeradarctl-x86_64-apple-darwin.tar.zst"),
        1
    );
}

#[test]
fn rejects_stream_overflow_short_stream_and_digest_mismatch_without_partial_files() {
    for bytes in [
        b"planeradar-binary!".to_vec(),
        b"planeradar-binar".to_vec(),
        b"planeradar-binarx".to_vec(),
    ] {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let cache = temporary.path().join("cache");
        let source = FakeSource::with_release(VALID_MANIFEST.as_bytes().to_vec());
        source.set("planeradar-aarch64-linux-gnu.tar.zst", bytes);

        assert!(resolve_downloaded(source, &cache).is_err());
        let final_path = cache
            .join("artifacts")
            .join("a8b3f6f4320547c3ef85f3860638f2f0156459307aa4d9e7c369cb8917ace9da")
            .join("planeradar-aarch64-linux-gnu.tar.zst");
        assert!(!final_path.exists());
        if let Some(parent) = final_path.parent() {
            let names = fs::read_dir(parent)
                .map(|entries| {
                    entries
                        .map(|entry| {
                            entry
                                .expect("cache entry")
                                .file_name()
                                .to_string_lossy()
                                .into_owned()
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            assert!(names.is_empty(), "partial files leaked: {names:?}");
        }
    }
}

#[test]
fn rejects_corrupt_insecure_and_symlinked_cache_hits() {
    for mode in ["corrupt", "insecure", "symlink", "hardlink"] {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let cache = temporary.path().join("cache");
        let source = FakeSource::with_release(VALID_MANIFEST.as_bytes().to_vec());
        let resolved = resolve_downloaded(source.clone(), &cache).expect("prime cache");
        let path = resolved.artifacts[0].path.clone();
        match mode {
            "corrupt" => fs::write(&path, b"planeradar-binarx").expect("corrupt cached file"),
            "insecure" => fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
                .expect("make cache file public"),
            "symlink" => {
                let outside = temporary.path().join("outside");
                fs::write(&outside, APP_BYTES).expect("outside bytes");
                fs::remove_file(&path).expect("remove cache file");
                symlink(&outside, &path).expect("cache symlink");
            }
            "hardlink" => {
                let alias = temporary.path().join("retained-cache-inode");
                fs::hard_link(&path, alias).expect("cache hard link");
                assert_eq!(fs::metadata(&path).expect("metadata").nlink(), 2);
            }
            _ => unreachable!(),
        }
        assert!(
            resolve_downloaded(source, &cache).is_err(),
            "{mode} cache hit was accepted"
        );
    }
}

#[test]
fn rejects_existing_cache_components_with_any_group_or_other_permissions() {
    for mode in [0o750, 0o707, 0o777] {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let cache = temporary.path().join("cache");
        fs::create_dir(&cache).expect("cache root");
        fs::set_permissions(&cache, fs::Permissions::from_mode(mode)).expect("set unsafe mode");
        let source = FakeSource::with_release(VALID_MANIFEST.as_bytes().to_vec());
        assert!(
            resolve_downloaded(source, &cache).is_err(),
            "cache mode {mode:o} was accepted"
        );
    }
}

#[test]
fn rejects_symlinked_cache_components() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let cache = temporary.path().join("cache");
    let outside = temporary.path().join("outside");
    fs::create_dir(&cache).expect("cache root");
    fs::create_dir(&outside).expect("outside directory");
    symlink(&outside, cache.join("artifacts")).expect("cache component symlink");
    let source = FakeSource::with_release(VALID_MANIFEST.as_bytes().to_vec());

    assert!(resolve_downloaded(source, &cache).is_err());
    assert!(
        fs::read_dir(&outside)
            .expect("outside entries")
            .next()
            .is_none()
    );
}

#[test]
fn caps_downloaded_manifest_before_json_parsing() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let source = FakeSource::with_release(vec![b' '; 65_537]);
    assert!(resolve_downloaded(source, &temporary.path().join("cache")).is_err());
}

#[derive(Default)]
struct RecordingStreamingRunner {
    invocations: Mutex<Vec<Invocation>>,
    streams: Mutex<HashMap<String, Vec<u8>>>,
}

impl RecordingStreamingRunner {
    fn set(&self, name: &str, bytes: Vec<u8>) {
        self.streams
            .lock()
            .expect("stream map")
            .insert(name.into(), bytes);
    }
}

impl StreamingCommandRunner for &RecordingStreamingRunner {
    fn run_streaming(
        &self,
        invocation: Invocation,
        sink: &mut dyn Write,
    ) -> Result<(), ReleaseSourceError> {
        let name = invocation
            .arguments()
            .get(4)
            .cloned()
            .ok_or(ReleaseSourceError::Failed)?;
        self.invocations
            .lock()
            .expect("stream invocations")
            .push(invocation);
        sink.write_all(
            self.streams
                .lock()
                .expect("stream map")
                .get(&name)
                .ok_or(ReleaseSourceError::Failed)?,
        )
        .map_err(|_| ReleaseSourceError::Failed)
    }
}

#[test]
fn production_release_source_builds_fixed_no_shell_gh_download_vectors() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let runner = RecordingStreamingRunner::default();
    let mut stable_manifest = valid_value();
    set_version(&mut stable_manifest, "0.1.0");
    set_path(
        &mut stable_manifest,
        "/workflow/path",
        json!(".github/workflows/stable-draft.yml"),
    );
    runner.set(
        "release-manifest.json",
        serde_json::to_vec(&stable_manifest).expect("stable manifest"),
    );
    runner.set("planeradar-aarch64-linux-gnu.tar.zst", APP_BYTES.to_vec());
    runner.set(
        "planeradarctl-aarch64-apple-darwin.tar.zst",
        CONTROL_ARM64_BYTES.to_vec(),
    );
    runner.set(
        "planeradarctl-x86_64-apple-darwin.tar.zst",
        CONTROL_X86_64_BYTES.to_vec(),
    );
    let source = GhReleaseSource::new(&runner);

    ReleaseClient::new(source, temporary.path().join("cache"))
        .resolve(&requested("0.1.0"), &lock(), ReleaseInput::Downloaded)
        .expect("resolve through gh source");

    let invocations = runner.invocations.lock().expect("invocations");
    assert_eq!(invocations.len(), 4);
    for (index, name) in [
        "release-manifest.json",
        "planeradar-aarch64-linux-gnu.tar.zst",
        "planeradarctl-aarch64-apple-darwin.tar.zst",
        "planeradarctl-x86_64-apple-darwin.tar.zst",
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(invocations[index].program(), "gh");
        assert_eq!(
            invocations[index].arguments(),
            [
                "release",
                "download",
                "v0.1.0",
                "--pattern",
                *name,
                "--output",
                "-",
                "-R",
                "shayne/RPi-Plane-Radar",
            ]
        );
    }
}

#[test]
fn system_streaming_runner_executes_a_typed_program_and_propagates_failure() {
    let runner = SystemStreamingCommandRunner;
    let mut stdout = Vec::new();
    runner
        .run_streaming(
            Invocation::new("/usr/bin/printf", vec!["streamed".into()]),
            &mut stdout,
        )
        .expect("direct typed process");
    assert_eq!(stdout, b"streamed");
    assert!(
        runner
            .run_streaming(
                Invocation::new("/usr/bin/false", Vec::new()),
                &mut Vec::new()
            )
            .is_err()
    );
}

#[derive(Default)]
struct RecordingCommandRunner {
    invocations: Mutex<Vec<Invocation>>,
    statuses: Mutex<Vec<i32>>,
}

impl RecordingCommandRunner {
    fn with_statuses(statuses: Vec<i32>) -> Self {
        Self {
            invocations: Mutex::new(Vec::new()),
            statuses: Mutex::new(statuses.into_iter().rev().collect()),
        }
    }
}

impl CommandRunner for &RecordingCommandRunner {
    fn run(&self, invocation: Invocation) -> Result<CommandOutput, RunnerError> {
        self.invocations
            .lock()
            .expect("command invocations")
            .push(invocation);
        let status = self
            .statuses
            .lock()
            .expect("command statuses")
            .pop()
            .unwrap_or(0);
        Ok(CommandOutput::new(
            status,
            b"future secret stdout".to_vec(),
            b"future secret stderr".to_vec(),
        ))
    }
}

struct MutatingCommandRunner {
    invocations: Mutex<Vec<Invocation>>,
    path: PathBuf,
    mutate_on_call: usize,
    replacement: Vec<u8>,
}

impl MutatingCommandRunner {
    fn new(path: PathBuf, mutate_on_call: usize, replacement: &[u8]) -> Self {
        Self {
            invocations: Mutex::new(Vec::new()),
            path,
            mutate_on_call,
            replacement: replacement.to_vec(),
        }
    }
}

impl CommandRunner for &MutatingCommandRunner {
    fn run(&self, invocation: Invocation) -> Result<CommandOutput, RunnerError> {
        let mut invocations = self.invocations.lock().expect("mutating invocations");
        invocations.push(invocation);
        if invocations.len() == self.mutate_on_call {
            fs::write(&self.path, &self.replacement).expect("persistent mutation");
            fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))
                .expect("preserve private mode");
        }
        Ok(CommandOutput::success(Vec::new(), Vec::new()))
    }
}

fn stable_resolved(root: &Path) -> ResolvedRelease {
    let mut value = valid_value();
    set_version(&mut value, "0.1.0");
    set_path(
        &mut value,
        "/workflow/path",
        json!(".github/workflows/stable-draft.yml"),
    );
    let manifest = parse_value(&value, "0.1.0").expect("stable manifest");
    resolved_from_manifest(root, manifest)
}

fn resolved_from_manifest(root: &Path, manifest: ReleaseManifest) -> ResolvedRelease {
    fs::create_dir_all(root).expect("resolved artifact root");
    ResolvedRelease {
        tag: format!("v{}", manifest.version),
        manifest: manifest.clone(),
        artifacts: manifest
            .artifacts
            .iter()
            .cloned()
            .map(|artifact| {
                let path = root.join(&artifact.name);
                let bytes = match artifact.name.as_str() {
                    "planeradar-aarch64-linux-gnu.tar.zst" => APP_BYTES,
                    "planeradarctl-aarch64-apple-darwin.tar.zst" => CONTROL_ARM64_BYTES,
                    "planeradarctl-x86_64-apple-darwin.tar.zst" => CONTROL_X86_64_BYTES,
                    _ => panic!("unexpected test artifact"),
                };
                fs::write(&path, bytes).expect("resolved artifact");
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
                    .expect("private resolved artifact");
                ResolvedArtifact { path, artifact }
            })
            .collect(),
    }
}

#[test]
fn stable_release_verification_uses_exact_gh_vectors_for_every_runnable_artifact() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let runner = RecordingCommandRunner::default();
    let release = stable_resolved(temporary.path());

    Verifier::new(&runner)
        .verify(&requested("0.1.0"), &release)
        .expect("stable verification");

    let invocations = runner.invocations.lock().expect("invocations");
    assert_eq!(invocations.len(), 4);
    assert_eq!(invocations[0].program(), "gh");
    assert_eq!(
        invocations[0].arguments(),
        [
            "release",
            "verify",
            "v0.1.0",
            "-R",
            "shayne/RPi-Plane-Radar"
        ]
    );
    for (invocation, name) in invocations[1..].iter().zip([
        "planeradar-aarch64-linux-gnu.tar.zst",
        "planeradarctl-aarch64-apple-darwin.tar.zst",
        "planeradarctl-x86_64-apple-darwin.tar.zst",
    ]) {
        assert_eq!(invocation.program(), "gh");
        assert_eq!(
            invocation.os_arguments(),
            [
                "attestation".into(),
                "verify".into(),
                temporary.path().join(name).into_os_string(),
                "-R".into(),
                "shayne/RPi-Plane-Radar".into(),
                "--signer-workflow".into(),
                "shayne/RPi-Plane-Radar/.github/workflows/stable-draft.yml".into(),
                "--source-ref".into(),
                "refs/heads/main".into(),
                "--source-digest".into(),
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
                "--deny-self-hosted-runners".into(),
            ]
        );
    }
    assert!(invocations.iter().all(|invocation| {
        invocation.program() != "sh"
            && invocation.program() != "bash"
            && !invocation
                .arguments()
                .iter()
                .any(|argument| argument == "-c")
    }));
}

#[test]
fn stable_verification_rejects_persistent_mutation_during_release_verification() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let release = stable_resolved(temporary.path());
    let runnable = release
        .artifacts
        .iter()
        .find(|artifact| artifact.artifact.runnable)
        .expect("runnable artifact");
    let runner = MutatingCommandRunner::new(runnable.path.clone(), 1, b"planeradar-binarx");

    assert!(
        Verifier::new(&runner)
            .verify(&requested("0.1.0"), &release)
            .is_err()
    );
    assert_eq!(runner.invocations.lock().expect("invocations").len(), 1);
}

#[test]
fn stable_verification_rejects_persistent_mutation_during_attestation() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let release = stable_resolved(temporary.path());
    let runnable = release
        .artifacts
        .iter()
        .find(|artifact| artifact.artifact.runnable)
        .expect("runnable artifact");
    let runner = MutatingCommandRunner::new(runnable.path.clone(), 2, b"planeradar-binarx");

    assert!(
        Verifier::new(&runner)
            .verify(&requested("0.1.0"), &release)
            .is_err()
    );
    assert_eq!(runner.invocations.lock().expect("invocations").len(), 2);
}

#[test]
fn stable_verification_final_pass_rejects_later_runnable_mutation() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let release = stable_resolved(temporary.path());
    let later = release.artifacts.last().expect("later runnable artifact");
    let runner = MutatingCommandRunner::new(later.path.clone(), 2, b"control-x86_64-bix");

    assert!(
        Verifier::new(&runner)
            .verify(&requested("0.1.0"), &release)
            .is_err()
    );
    assert_eq!(runner.invocations.lock().expect("invocations").len(), 3);
}

#[test]
fn stable_build_metadata_is_not_mistaken_for_a_prerelease() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let mut value = valid_value();
    set_version(&mut value, "0.1.0+build.7");
    set_path(
        &mut value,
        "/workflow/path",
        json!(".github/workflows/stable-draft.yml"),
    );
    let manifest = parse_value(&value, "0.1.0+build.7").expect("build metadata manifest");
    let release = resolved_from_manifest(temporary.path(), manifest);
    let runner = RecordingCommandRunner::default();

    Verifier::new(&runner)
        .verify(&requested("0.1.0+build.7"), &release)
        .expect("stable build verification");

    assert_eq!(runner.invocations.lock().expect("invocations").len(), 4);
}

#[test]
fn prerelease_resolution_explicitly_skips_stable_attestation_policy() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let manifest =
        ReleaseManifest::parse(VALID_MANIFEST.as_bytes(), &requested("0.1.0-rc.1"), &lock())
            .expect("prerelease manifest");
    let release = resolved_from_manifest(temporary.path(), manifest);
    let runner = RecordingCommandRunner::with_statuses(vec![1]);

    Verifier::new(&runner)
        .verify(&requested("0.1.0-rc.1"), &release)
        .expect("prerelease does not enforce stable attestations");

    assert!(runner.invocations.lock().expect("invocations").is_empty());
}

#[test]
fn prerelease_verification_revalidates_cached_bytes_before_skipping_attestations() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let manifest =
        ReleaseManifest::parse(VALID_MANIFEST.as_bytes(), &requested("0.1.0-rc.1"), &lock())
            .expect("prerelease manifest");
    let release = resolved_from_manifest(temporary.path(), manifest);
    fs::write(&release.artifacts[0].path, b"planeradar-binarx")
        .expect("tamper cached artifact with same-size bytes");
    let runner = RecordingCommandRunner::default();

    assert!(
        Verifier::new(&runner)
            .verify(&requested("0.1.0-rc.1"), &release)
            .is_err()
    );
    assert!(runner.invocations.lock().expect("invocations").is_empty());
}

#[test]
fn verifier_excludes_non_runnable_assets_and_fails_closed_on_command_error() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let release = stable_resolved(temporary.path());
    let release_failure = RecordingCommandRunner::with_statuses(vec![1]);
    assert!(
        Verifier::new(&release_failure)
            .verify(&requested("0.1.0"), &release)
            .is_err()
    );
    assert_eq!(
        release_failure
            .invocations
            .lock()
            .expect("invocations")
            .len(),
        1
    );

    let attestation_failure = RecordingCommandRunner::with_statuses(vec![0, 1]);
    assert!(
        Verifier::new(&attestation_failure)
            .verify(&requested("0.1.0"), &release)
            .is_err()
    );
    let invocations = attestation_failure.invocations.lock().expect("invocations");
    assert_eq!(invocations.len(), 2);
    assert!(invocations.iter().all(|invocation| {
        !invocation
            .os_arguments()
            .contains(&temporary.path().join("sbom.spdx.json").into_os_string())
    }));
}

#[test]
fn verifier_rejects_requested_manifest_and_tag_identity_mismatches() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let runner = RecordingCommandRunner::default();
    let mut release = stable_resolved(temporary.path());
    assert!(
        Verifier::new(&runner)
            .verify(&requested("0.1.1"), &release)
            .is_err()
    );
    release.tag = "v0.1.1".into();
    assert!(
        Verifier::new(&runner)
            .verify(&requested("0.1.0"), &release)
            .is_err()
    );
    assert!(runner.invocations.lock().expect("invocations").is_empty());
}

#[test]
fn debug_and_errors_redact_manifest_bytes_command_output_and_local_paths() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let secret_path = temporary.path().join("maintainer-secret-layout");
    let client = ReleaseClient::new(FakeSource::default(), secret_path.clone());
    let client_debug = format!("{client:?}");
    assert!(!client_debug.contains(&secret_path.to_string_lossy().into_owned()));

    let release = stable_resolved(&secret_path);
    let release_debug = format!("{release:?}");
    assert!(!release_debug.contains(&secret_path.to_string_lossy().into_owned()));
    assert!(
        !format!("{:?}", release.manifest)
            .contains("8170e79829fb5969fffa280c2b971f37bcb757af62a57997b4d5a90bf88d8b02")
    );

    let runner = RecordingCommandRunner::with_statuses(vec![1]);
    let error = Verifier::new(&runner)
        .verify(&requested("0.1.0"), &release)
        .expect_err("verification failure");
    for rendered in [format!("{error}"), format!("{error:?}")] {
        assert!(!rendered.contains("future secret stdout"));
        assert!(!rendered.contains("future secret stderr"));
        assert!(!rendered.contains(&secret_path.to_string_lossy().into_owned()));
    }
}

#[test]
fn download_request_is_fixed_to_the_application_repository_and_exact_tag() {
    let request = DownloadRequest::new(&requested("0.1.0"), "planeradar-aarch64-linux-gnu.tar.zst")
        .expect("safe request");
    assert_eq!(request.repository(), APP_REPOSITORY);
    assert_eq!(request.tag(), "v0.1.0");
    assert_eq!(request.name(), "planeradar-aarch64-linux-gnu.tar.zst");
    assert!(DownloadRequest::new(&requested("0.1.0"), "-output").is_err());
    assert!(DownloadRequest::new(&requested("0.1.0"), "glob*").is_err());
}
