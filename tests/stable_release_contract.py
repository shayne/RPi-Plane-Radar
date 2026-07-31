#!/usr/bin/env python3
"""Hostile, stateful simulations of stable draft creation and promotion."""

from __future__ import annotations

import copy
import hashlib
import importlib.util
import inspect
import json
import os
import pathlib
import subprocess
import sys
import tempfile


sys.dont_write_bytecode = True
ROOT = pathlib.Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "scripts" / "stable_release.py"
SPEC = importlib.util.spec_from_file_location("stable_release", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise SystemExit("cannot load scripts/stable_release.py")
stable_release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(stable_release)
download_source = inspect.getsource(stable_release.GhGitBackend.download_asset)
assert "stdout=subprocess.PIPE" not in download_source
assert "RLIMIT_FSIZE" in download_source
assert "O_EXCL" in download_source

TAG = "v0.1.0"
COMMIT = "a" * 40
TAGGER_TIMESTAMP = 1700000000
TAG_OBJECT = stable_release.canonical_tag_object(TAG, COMMIT, TAGGER_TIMESTAMP)
assert len(TAG_OBJECT) == 40


def exercise_real_upload_endpoint_contract() -> None:
    calls: list[list[str]] = []
    original_run = stable_release.subprocess.run

    def fake_run(
        arguments: list[str], **kwargs: object
    ) -> subprocess.CompletedProcess:
        calls.append(arguments)
        return subprocess.CompletedProcess(
            arguments,
            0,
            stdout=(
                b'{"id":100,"name":"SBOM.spdx.json","size":7,'
                b'"digest":"sha256:'
                + b"0" * 64
                + b'"}'
            ),
        )

    stable_release.subprocess.run = fake_run
    try:
        with tempfile.TemporaryDirectory(prefix="planeradar-upload-endpoint.") as temporary:
            asset = pathlib.Path(temporary) / "SBOM.spdx.json"
            asset.write_bytes(b"fixture")
            stable_release.GhGitBackend().upload_asset(
                42, "SBOM.spdx.json", asset
            )
    finally:
        stable_release.subprocess.run = original_run

    assert len(calls) == 1
    arguments = calls[0]
    assert (
        "https://uploads.github.com/repos/shayne/RPi-Plane-Radar/"
        "releases/42/assets?name=SBOM.spdx.json"
    ) in arguments


exercise_real_upload_endpoint_contract()


def exercise_real_optional_release_lookup_contract() -> None:
    original_run = stable_release.subprocess.run
    responses = [
        subprocess.CompletedProcess(
            ["gh"],
            1,
            stdout=b"",
            stderr=b"gh: Not Found (HTTP 404)\n",
        ),
        subprocess.CompletedProcess(
            ["gh"],
            0,
            stdout=b'{"id":42,"draft":true}',
            stderr=b"",
        ),
    ]

    def fake_run(
        arguments: list[str], **kwargs: object
    ) -> subprocess.CompletedProcess:
        assert arguments[-1] == "repos/shayne/RPi-Plane-Radar/releases/42"
        return responses.pop(0)

    stable_release.subprocess.run = fake_run
    try:
        backend = stable_release.GhGitBackend()
        assert backend.get_release_optional(42) is None
        assert backend.get_release_optional(42) == {"id": 42, "draft": True}
    finally:
        stable_release.subprocess.run = original_run
    assert not responses


exercise_real_optional_release_lookup_contract()


class FakeBackend:
    def __init__(self) -> None:
        self.release: dict | None = None
        self.asset_bytes: dict[int, bytes] = {}
        self.remote_tags: dict[str, tuple[str, str]] = {}
        self.local_tags: dict[str, tuple[str, str]] = {}
        self.upload_count = 0
        self.build_count = 0
        self.patch_payloads: list[dict] = []
        self.next_asset_id = 100
        self.push_mode = "success"
        self.patch_mode = "success"
        self.release_delete_mode = "success"
        self.delete_mode = "success"
        self.discard_mode = "success"
        self.fail_next_remote_read = False
        self.partial_asset: int | None = None
        self.extra_asset: int | None = None
        self.download_limits: list[tuple[int, int]] = []

    def remote_tag(self, tag: str) -> dict | None:
        if self.fail_next_remote_read:
            self.fail_next_remote_read = False
            raise OSError("fixture remote tag reread failed")
        value = self.remote_tags.get(tag)
        if value is None:
            return None
        return {"object": value[0], "commit": value[1]}

    def commit_timestamp(self, commit: str) -> int:
        assert commit == COMMIT
        return TAGGER_TIMESTAMP

    def create_release(self, tag: str, commit: str) -> dict:
        if self.release is not None:
            raise stable_release.ContractError("release already exists")
        self.release = {
            "id": 42,
            "tag_name": tag,
            "target_commitish": commit,
            "draft": True,
            "prerelease": False,
            "immutable": False,
            "assets": [],
        }
        return copy.deepcopy(self.release)

    def delete_release(self, release_id: int) -> None:
        if self.release_delete_mode == "before-success":
            raise OSError("fixture release delete failed before success")
        if self.release and self.release["id"] == release_id:
            self.release = None
            self.asset_bytes.clear()
        if self.release_delete_mode == "after-success":
            raise OSError("fixture release delete response was lost")

    def get_release_optional(self, release_id: int) -> dict | None:
        if not self.release or self.release["id"] != release_id:
            return None
        return copy.deepcopy(self.release)

    def upload_asset(self, release_id: int, name: str, path: pathlib.Path) -> dict:
        assert self.release and self.release["id"] == release_id
        data = path.read_bytes()
        self.upload_count += 1
        asset = {
            "id": self.next_asset_id,
            "name": name,
            "size": len(data),
            "digest": f"sha256:{hashlib.sha256(data).hexdigest()}",
        }
        self.next_asset_id += 1
        self.release["assets"].append(asset)
        self.asset_bytes[asset["id"]] = data
        return copy.deepcopy(asset)

    def get_release(self, release_id: int) -> dict:
        if not self.release or self.release["id"] != release_id:
            raise stable_release.ContractError("release missing")
        return copy.deepcopy(self.release)

    def download_asset(
        self, asset_id: int, destination: pathlib.Path, byte_limit: int
    ) -> dict:
        data = self.asset_bytes[asset_id]
        if self.extra_asset == asset_id:
            data += b"unexpected extra response bytes"
        self.download_limits.append((asset_id, byte_limit))
        descriptor = os.open(
            destination,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0),
            0o600,
        )
        digest = hashlib.sha256()
        written = 0
        try:
            with os.fdopen(descriptor, "wb") as output:
                for offset in range(0, len(data), 3):
                    chunk = data[offset : offset + 3]
                    allowed = byte_limit - written
                    if allowed <= 0:
                        raise stable_release.ContractError("fixture byte ceiling reached")
                    if len(chunk) > allowed:
                        output.write(chunk[:allowed])
                        digest.update(chunk[:allowed])
                        written += allowed
                        raise stable_release.ContractError("fixture byte ceiling reached")
                    output.write(chunk)
                    digest.update(chunk)
                    written += len(chunk)
                    if self.partial_asset == asset_id and written >= 2:
                        raise OSError("fixture partial transport failure")
        except Exception:
            raise
        return {"size": written, "digest": f"sha256:{digest.hexdigest()}"}

    def ensure_local_annotated_tag(
        self, tag: str, commit: str, tagger_timestamp: int, tag_object: str
    ) -> str:
        assert tagger_timestamp == TAGGER_TIMESTAMP
        existing = self.local_tags.get(tag)
        if existing is not None and existing != (tag_object, commit):
            raise stable_release.ContractError("different local tag already exists")
        self.local_tags[tag] = (tag_object, commit)
        return tag_object

    def push_annotated_tag(self, tag: str, tag_object: str) -> None:
        assert self.local_tags.get(tag) == (tag_object, COMMIT)
        if self.push_mode == "before-success":
            raise OSError("fixture push failed before success")
        if self.push_mode == "wrong-remote":
            self.remote_tags[tag] = ("e" * 40, "f" * 40)
            raise OSError("fixture push raced a different remote tag")
        self.remote_tags[tag] = self.local_tags[tag]
        if self.push_mode == "after-success":
            raise OSError("fixture push response was lost")

    def publish_release(self, release_id: int) -> dict:
        assert self.release and self.release["id"] == release_id
        payload = {"draft": False, "prerelease": False}
        self.patch_payloads.append(payload)
        if self.patch_mode == "before-error":
            raise OSError("fixture PATCH failed before success")
        self.release.update(payload)
        self.release["immutable"] = True
        if self.patch_mode == "mutable-publication":
            self.release["immutable"] = False
        if self.patch_mode == "after-success-error":
            raise OSError("fixture PATCH response was lost")
        if self.patch_mode == "invalid-success-published":
            return {"invalid": True}
        if self.patch_mode == "invalid-success-draft":
            self.release.update({"draft": True, "prerelease": False})
            return {"invalid": True}
        if self.patch_mode == "drift":
            self.release["assets"][0]["digest"] = f"sha256:{'0' * 64}"
            raise OSError("fixture PATCH failed with release drift")
        if self.patch_mode == "tag-race":
            self.remote_tags[TAG] = ("e" * 40, "f" * 40)
        return copy.deepcopy(self.release)

    def delete_remote_annotated_tag(
        self, tag: str, commit: str, tag_object: str
    ) -> None:
        if self.remote_tags.get(tag) != (tag_object, commit):
            raise stable_release.ContractError("refusing to delete a different tag")
        if self.delete_mode == "before-success":
            raise OSError("fixture delete failed before success")
        if self.delete_mode == "drift":
            self.remote_tags[tag] = ("e" * 40, commit)
            raise OSError("fixture delete raced a different tag")
        del self.remote_tags[tag]
        if self.delete_mode == "after-success":
            raise OSError("fixture delete response was lost")
        if self.delete_mode == "reread-error":
            self.fail_next_remote_read = True

    def discard_local_tag(self, tag: str, commit: str, tag_object: str) -> None:
        if self.discard_mode == "error":
            raise OSError("fixture local cleanup failed")
        existing = self.local_tags.get(tag)
        if existing is None:
            return
        if existing != (tag_object, commit):
            raise stable_release.ContractError("refusing to discard a different local tag")
        del self.local_tags[tag]


def fixture_assets(directory: pathlib.Path) -> None:
    files = {
        "SHA256SUMS": b"fixture checksums\n",
        "SBOM.spdx.json": b'{"spdxVersion":"SPDX-2.3"}\n',
        "install.sh": b"#!/bin/sh\n",
        "planeradar-aarch64-linux-gnu.tar.zst": b"app\n",
        "planeradarctl-aarch64-apple-darwin.tar.zst": b"control arm64\n",
        "planeradarctl-x86_64-apple-darwin.tar.zst": b"control x86_64\n",
        "release-manifest.json": json.dumps(
            {"source_commit": COMMIT, "source_tree": "b" * 40}
        ).encode(),
    }
    for name, data in files.items():
        (directory / name).write_bytes(data)


def expect_rejected(label: str, function) -> None:
    try:
        function()
    except stable_release.ContractError:
        return
    raise AssertionError(f"stable release accepted hostile state: {label}")


def exercise_real_git_local_tag_contract() -> None:
    with tempfile.TemporaryDirectory(prefix="planeradar-stable-real-git.") as temporary:
        repository = pathlib.Path(temporary) / "working"
        repository.mkdir()

        def git(*arguments: str, check: bool = True) -> subprocess.CompletedProcess:
            return subprocess.run(
                ["git", *arguments],
                cwd=repository,
                text=True,
                stdout=subprocess.PIPE,
                check=check,
            )

        git("init", "--quiet")
        git("config", "user.name", "Fixture Author")
        git("config", "user.email", "fixture@example.invalid")
        (repository / "fixture.txt").write_text("canonical tag fixture\n")
        git("add", "fixture.txt")
        commit_environment = os.environ.copy()
        commit_environment.update(
            {
                "GIT_AUTHOR_DATE": "1700000000 +0000",
                "GIT_COMMITTER_DATE": "1700000000 +0000",
            }
        )
        subprocess.run(
            ["git", "commit", "--quiet", "--message", "fixture"],
            cwd=repository,
            env=commit_environment,
            check=True,
        )
        commit = git("rev-parse", "HEAD").stdout.strip()

        previous = pathlib.Path.cwd()
        os.chdir(repository)
        try:
            backend = stable_release.GhGitBackend()
            timestamp = backend.commit_timestamp(commit)
            tag_object = stable_release.canonical_tag_object(
                TAG, commit, timestamp
            )
            created = backend.ensure_local_annotated_tag(
                TAG, commit, timestamp, tag_object
            )
            assert created == tag_object
            assert git("rev-parse", f"refs/tags/{TAG}").stdout.strip() == tag_object
            assert git("cat-file", "-t", tag_object).stdout.strip() == "tag"
            reused = backend.ensure_local_annotated_tag(
                TAG, commit, timestamp, tag_object
            )
            assert reused == tag_object

            backend.discard_local_tag(TAG, commit, tag_object)
            assert (
                git(
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    f"refs/tags/{TAG}",
                    check=False,
                ).returncode
                == 1
            )

            git("update-ref", f"refs/tags/{TAG}", commit)
            wrong_object = git("rev-parse", f"refs/tags/{TAG}").stdout.strip()
            assert wrong_object == commit
            assert wrong_object != tag_object
            expect_rejected(
                "real Git local tag has a different object",
                lambda: backend.discard_local_tag(TAG, commit, tag_object),
            )
            assert (
                git("rev-parse", f"refs/tags/{TAG}").stdout.strip()
                == wrong_object
            )
        finally:
            os.chdir(previous)


exercise_real_git_local_tag_contract()


with tempfile.TemporaryDirectory(prefix="planeradar-stable-release.") as temporary:
    root = pathlib.Path(temporary)
    assets = root / "assets"
    assets.mkdir()
    fixture_assets(assets)
    backend = FakeBackend()

    record = stable_release.create_stable_draft(backend, TAG, COMMIT, assets)
    assert backend.remote_tag(TAG) is None
    assert record["release_id"] == 42
    assert record["tag"] == TAG
    assert record["commit"] == COMMIT
    assert record["asset_fingerprint"]
    assert backend.upload_count == 7

    downloads = root / "downloads"
    verified = stable_release.verify_stable_draft(
        backend,
        TAG,
        COMMIT,
        record["release_id"],
        record["asset_fingerprint"],
        downloads,
    )
    assert sorted(path.name for path in downloads.iterdir()) == sorted(
        stable_release.REQUIRED_ASSETS
    )
    uploads_before_promotion = backend.upload_count
    published = stable_release.publish_verified_draft(backend, verified, downloads)
    assert published["schema_version"] == 2
    assert published["tag_object"] == TAG_OBJECT
    assert published["tagger_timestamp"] == TAGGER_TIMESTAMP
    assert backend.remote_tag(TAG) == {"object": TAG_OBJECT, "commit": COMMIT}
    assert backend.upload_count == uploads_before_promotion
    assert backend.build_count == 0
    assert backend.patch_payloads == [{"draft": False, "prerelease": False}]
    assert backend.release and backend.release["draft"] is False
    assert backend.release["prerelease"] is False
    stable_release.confirm_published(backend, published)
    backend.remote_tags[TAG] = ("e" * 40, COMMIT)
    expect_rejected(
        "published tag object replaced without changing peeled commit",
        lambda: stable_release.confirm_published(backend, published),
    )
    backend.remote_tags[TAG] = (TAG_OBJECT, COMMIT)

    def staged_backend() -> tuple[FakeBackend, dict]:
        candidate = FakeBackend()
        candidate_record = stable_release.create_stable_draft(
            candidate, TAG, COMMIT, assets
        )
        return candidate, candidate_record

    candidate, candidate_record = staged_backend()
    candidate.remote_tags[TAG] = ("d" * 40, COMMIT)
    expect_rejected(
        "preexisting stable tag",
        lambda: stable_release.verify_stable_draft(
            candidate,
            TAG,
            COMMIT,
            candidate_record["release_id"],
            candidate_record["asset_fingerprint"],
            root / "tag-exists",
        ),
    )

    # The one canonical tag object is a durable retry state even on a fresh
    # runner.  Any other annotated object peeling to the same commit is not.
    candidate, candidate_record = staged_backend()
    candidate.remote_tags[TAG] = (TAG_OBJECT, COMMIT)
    canonical_resume_downloads = root / "canonical-tag-resume"
    canonical_resume = stable_release.verify_stable_draft(
        candidate,
        TAG,
        COMMIT,
        candidate_record["release_id"],
        candidate_record["asset_fingerprint"],
        canonical_resume_downloads,
    )
    stable_release.publish_verified_draft(
        candidate, canonical_resume, canonical_resume_downloads
    )
    assert candidate.release and candidate.release["draft"] is False

    for label, mutation in (
        ("missing asset", lambda release: release["assets"].pop()),
        (
            "extra asset",
            lambda release: release["assets"].append(
                {
                    "id": 999,
                    "name": "foreign.bin",
                    "size": 1,
                    "digest": f"sha256:{'0' * 64}",
                }
            ),
        ),
        (
            "replaced asset identity",
            lambda release: release["assets"][0].__setitem__("id", 999),
        ),
        (
            "replaced asset digest",
            lambda release: release["assets"][0].__setitem__(
                "digest", f"sha256:{'0' * 64}"
            ),
        ),
        (
            "wrong source commit",
            lambda release: release.__setitem__("target_commitish", "c" * 40),
        ),
        ("not a draft", lambda release: release.__setitem__("draft", False)),
        ("prerelease draft", lambda release: release.__setitem__("prerelease", True)),
    ):
        candidate, candidate_record = staged_backend()
        assert candidate.release is not None
        mutation(candidate.release)
        rejected_downloads = root / label.replace(" ", "-")
        expect_rejected(
            label,
            lambda candidate=candidate, record=candidate_record: (
                stable_release.verify_stable_draft(
                    candidate,
                    TAG,
                    COMMIT,
                    record["release_id"],
                    record["asset_fingerprint"],
                    rejected_downloads,
                )
            ),
        )
        assert not rejected_downloads.exists()

    candidate, candidate_record = staged_backend()
    expect_rejected(
        "wrong accepted fingerprint",
        lambda: stable_release.verify_stable_draft(
            candidate,
            TAG,
            COMMIT,
            candidate_record["release_id"],
            "0" * 64,
            root / "wrong-fingerprint",
        ),
    )
    assert not (root / "wrong-fingerprint").exists()

    def accepted_candidate(label: str) -> tuple[FakeBackend, dict, pathlib.Path]:
        candidate = FakeBackend()
        candidate_record = stable_release.create_stable_draft(
            candidate, TAG, COMMIT, assets
        )
        candidate_downloads = root / label
        verified_record = stable_release.verify_stable_draft(
            candidate,
            TAG,
            COMMIT,
            candidate_record["release_id"],
            candidate_record["asset_fingerprint"],
            candidate_downloads,
        )
        return candidate, verified_record, candidate_downloads

    def fresh_runner(candidate: FakeBackend) -> FakeBackend:
        fresh = FakeBackend()
        fresh.release = copy.deepcopy(candidate.release)
        fresh.asset_bytes = copy.deepcopy(candidate.asset_bytes)
        fresh.remote_tags = copy.deepcopy(candidate.remote_tags)
        fresh.next_asset_id = candidate.next_asset_id
        return fresh

    # A local tag must never substitute for the exact remote public ref.
    candidate, verified, candidate_downloads = accepted_candidate("local-mask")
    candidate.local_tags[TAG] = ("d" * 40, COMMIT)
    assert candidate.remote_tag(TAG) is None
    expect_rejected(
        "local tag collision",
        lambda: stable_release.publish_verified_draft(
            candidate, verified, candidate_downloads
        ),
    )
    assert candidate.remote_tag(TAG) is None

    # Push failure before remote success discards only the exact local tag and
    # leaves a retryable draft.
    candidate, verified, candidate_downloads = accepted_candidate("push-before")
    candidate.push_mode = "before-success"
    expect_rejected(
        "push failed before success",
        lambda: stable_release.publish_verified_draft(
            candidate, verified, candidate_downloads
        ),
    )
    assert candidate.remote_tag(TAG) is None
    assert TAG not in candidate.local_tags
    candidate.push_mode = "success"
    stable_release.publish_verified_draft(candidate, verified, candidate_downloads)
    assert candidate.remote_tag(TAG) == {"object": TAG_OBJECT, "commit": COMMIT}

    # A lost successful push response is reconciled against the exact remote
    # annotated object and proceeds to publication.
    candidate, verified, candidate_downloads = accepted_candidate("push-after")
    candidate.push_mode = "after-success"
    stable_release.publish_verified_draft(candidate, verified, candidate_downloads)
    assert candidate.remote_tag(TAG) == {"object": TAG_OBJECT, "commit": COMMIT}
    assert candidate.release and candidate.release["draft"] is False

    # A different remote ref is ambiguous.  Do not delete or overwrite it.
    candidate, verified, candidate_downloads = accepted_candidate("push-race")
    candidate.push_mode = "wrong-remote"
    expect_rejected(
        "push raced a different remote tag",
        lambda: stable_release.publish_verified_draft(
            candidate, verified, candidate_downloads
        ),
    )
    assert candidate.remote_tag(TAG) == {"object": "e" * 40, "commit": "f" * 40}
    assert candidate.release and candidate.release["draft"] is True

    for mode, accepted in (
        ("after-success-error", True),
        ("invalid-success-published", True),
        ("before-error", False),
        ("invalid-success-draft", False),
    ):
        candidate, verified, candidate_downloads = accepted_candidate(f"patch-{mode}")
        candidate.patch_mode = mode
        if accepted:
            stable_release.publish_verified_draft(
                candidate, verified, candidate_downloads
            )
            assert candidate.remote_tag(TAG) == {
                "object": TAG_OBJECT,
                "commit": COMMIT,
            }
            assert candidate.release and candidate.release["draft"] is False
        else:
            expect_rejected(
                f"retryable PATCH outcome: {mode}",
                lambda candidate=candidate, verified=verified, directory=candidate_downloads: (
                    stable_release.publish_verified_draft(
                        candidate, verified, directory
                    )
                ),
            )
            assert candidate.remote_tag(TAG) is None
            assert TAG not in candidate.local_tags
            assert candidate.release and candidate.release["draft"] is True
            candidate.patch_mode = "success"
            stable_release.publish_verified_draft(
                candidate, verified, candidate_downloads
            )
            assert candidate.remote_tag(TAG) == {
                "object": TAG_OBJECT,
                "commit": COMMIT,
            }

    # Every compensating-delete outcome is reconciled by exact tag object,
    # peeled commit, and release state.  Each safe partial outcome can resume.
    for delete_mode in (
        "before-success",
        "after-success",
        "reread-error",
        "local-cleanup-error",
    ):
        candidate, verified, candidate_downloads = accepted_candidate(
            f"delete-{delete_mode}"
        )
        candidate.patch_mode = "before-error"
        if delete_mode == "local-cleanup-error":
            candidate.discard_mode = "error"
        else:
            candidate.delete_mode = delete_mode
        expect_rejected(
            f"compensating delete outcome: {delete_mode}",
            lambda candidate=candidate, verified=verified, directory=candidate_downloads: (
                stable_release.publish_verified_draft(candidate, verified, directory)
            ),
        )
        if delete_mode == "before-success":
            assert candidate.remote_tag(TAG) == {
                "object": TAG_OBJECT,
                "commit": COMMIT,
            }
        else:
            assert candidate.remote_tag(TAG) is None
        candidate.patch_mode = "success"
        candidate.delete_mode = "success"
        candidate.discard_mode = "success"
        resumed = stable_release.publish_verified_draft(
            candidate, verified, candidate_downloads
        )
        assert resumed["tag_object"] == TAG_OBJECT
        assert candidate.remote_tag(TAG) == {
            "object": TAG_OBJECT,
            "commit": COMMIT,
        }
        assert candidate.release and candidate.release["draft"] is False

    # The exact remote tag plus exact draft can resume without runner-local
    # state.  The fresh runner downloads and verifies the immutable assets.
    candidate, verified, candidate_downloads = accepted_candidate("fresh-runner-first")
    candidate.patch_mode = "before-error"
    candidate.delete_mode = "before-success"
    expect_rejected(
        "compensation retained exact canonical tag",
        lambda: stable_release.publish_verified_draft(
            candidate, verified, candidate_downloads
        ),
    )
    fresh = fresh_runner(candidate)
    fresh_downloads = root / "fresh-runner-downloads"
    fresh_verified = stable_release.verify_stable_draft(
        fresh,
        TAG,
        COMMIT,
        verified["release_id"],
        verified["asset_fingerprint"],
        fresh_downloads,
    )
    stable_release.publish_verified_draft(fresh, fresh_verified, fresh_downloads)
    assert fresh.remote_tag(TAG) == {"object": TAG_OBJECT, "commit": COMMIT}
    assert fresh.release and fresh.release["draft"] is False

    # A compensating delete that races a different annotated object must never
    # remove or adopt it, even when it peels to the accepted commit.
    candidate, verified, candidate_downloads = accepted_candidate("delete-drift")
    candidate.patch_mode = "before-error"
    candidate.delete_mode = "drift"
    expect_rejected(
        "compensating delete raced remote object drift",
        lambda: stable_release.publish_verified_draft(
            candidate, verified, candidate_downloads
        ),
    )
    assert candidate.remote_tag(TAG) == {"object": "e" * 40, "commit": COMMIT}
    expect_rejected(
        "retry refused drifted same-commit tag object",
        lambda: stable_release.publish_verified_draft(
            candidate, verified, candidate_downloads
        ),
    )

    for mode in ("drift", "tag-race", "mutable-publication"):
        candidate, verified, candidate_downloads = accepted_candidate(f"ambiguous-{mode}")
        candidate.patch_mode = mode
        expect_rejected(
            f"ambiguous publication state: {mode}",
            lambda candidate=candidate, verified=verified, directory=candidate_downloads: (
                stable_release.publish_verified_draft(candidate, verified, directory)
            ),
        )
        assert candidate.release is None
        if mode == "tag-race":
            assert candidate.remote_tag(TAG) == {
                "object": "e" * 40,
                "commit": "f" * 40,
            }
        else:
            assert candidate.remote_tag(TAG) is None

    candidate, verified, candidate_downloads = accepted_candidate(
        "lost-release-delete-response"
    )
    candidate.patch_mode = "drift"
    candidate.release_delete_mode = "after-success"
    expect_rejected(
        "lost release delete response",
        lambda: stable_release.publish_verified_draft(
            candidate, verified, candidate_downloads
        ),
    )
    assert candidate.release is None
    assert candidate.remote_tag(TAG) is None

    # Downloads stream to exclusive files.  A declared-size overrun or partial
    # transport failure removes every partial and permits an exact retry.
    for mode in ("oversized", "partial"):
        candidate, candidate_record = staged_backend()
        victim = candidate.release["assets"][0]["id"]
        if mode == "oversized":
            candidate.extra_asset = victim
        else:
            candidate.partial_asset = victim
        rejected = root / f"download-{mode}"
        expect_rejected(
            f"{mode} download",
            lambda candidate=candidate, record=candidate_record, rejected=rejected: (
                stable_release.verify_stable_draft(
                    candidate,
                    TAG,
                    COMMIT,
                    record["release_id"],
                    record["asset_fingerprint"],
                    rejected,
                )
            ),
        )
        assert not rejected.exists()
        candidate.extra_asset = None
        candidate.partial_asset = None
        stable_release.verify_stable_draft(
            candidate,
            TAG,
            COMMIT,
            candidate_record["release_id"],
            candidate_record["asset_fingerprint"],
            rejected,
        )
        assert rejected.is_dir()

    # Exercise the absolute per-file and aggregate ceilings with small test
    # limits.  The response body exceeds a valid declaration at the exact cap,
    # and cleanup must still leave no partial directory.
    original_asset_limit = stable_release.MAX_ASSET_SIZE
    original_total_limit = stable_release.MAX_TOTAL_SIZE
    try:
        for mode in ("per-file-ceiling", "aggregate-ceiling"):
            candidate, candidate_record = staged_backend()
            assert candidate.release
            inventory = stable_release._asset_inventory(candidate.release)
            if mode == "per-file-ceiling":
                stable_release.MAX_ASSET_SIZE = max(
                    asset["size"] for asset in inventory
                )
                victim_asset = max(inventory, key=lambda asset: asset["size"])
            else:
                stable_release.MAX_ASSET_SIZE = original_asset_limit
                stable_release.MAX_TOTAL_SIZE = sum(
                    asset["size"] for asset in inventory
                )
                victim_asset = inventory[-1]
            candidate.extra_asset = victim_asset["id"]
            rejected = root / f"download-{mode}"
            expect_rejected(
                f"response exceeds {mode}",
                lambda candidate=candidate, record=candidate_record, rejected=rejected: (
                    stable_release.verify_stable_draft(
                        candidate,
                        TAG,
                        COMMIT,
                        record["release_id"],
                        record["asset_fingerprint"],
                        rejected,
                    )
                ),
            )
            assert not rejected.exists()
            victim_limits = [
                limit
                for asset_id, limit in candidate.download_limits
                if asset_id == victim_asset["id"]
            ]
            assert victim_limits == [victim_asset["size"]]
    finally:
        stable_release.MAX_ASSET_SIZE = original_asset_limit
        stable_release.MAX_TOTAL_SIZE = original_total_limit

print("stable release hostile simulations passed")
