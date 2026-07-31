#!/usr/bin/env python3
"""Create, verify, and publish one immutable stable GitHub release draft."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import re
import resource
import stat
import subprocess
import sys
import tempfile
import urllib.parse
from typing import Any


REPOSITORY = "shayne/RPi-Plane-Radar"
REQUIRED_ASSETS = (
    "SHA256SUMS",
    "SBOM.spdx.json",
    "install.sh",
    "planeradar-aarch64-linux-gnu.tar.zst",
    "planeradarctl-aarch64-apple-darwin.tar.zst",
    "planeradarctl-x86_64-apple-darwin.tar.zst",
    "release-manifest.json",
)
MAX_ASSET_SIZE = 512 * 1024 * 1024
MAX_TOTAL_SIZE = 1024 * 1024 * 1024
STABLE_TAG = re.compile(r"^v[0-9]+\.[0-9]+\.[0-9]+$")
COMMIT = re.compile(r"^[0-9a-f]{40}$")
DIGEST = re.compile(r"^sha256:([0-9a-f]{64})$")
TAGGER_NAME = "github-actions[bot]"
TAGGER_EMAIL = "41898282+github-actions[bot]@users.noreply.github.com"


class ContractError(RuntimeError):
    """A stable-release precondition or immutable identity did not hold."""


def _require_identity(tag: str, commit: str) -> None:
    if not STABLE_TAG.fullmatch(tag):
        raise ContractError(f"stable tag is not canonical: {tag}")
    if not COMMIT.fullmatch(commit):
        raise ContractError("source commit must be a full lowercase SHA-1")


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _require_tagger_timestamp(timestamp: int) -> None:
    if isinstance(timestamp, bool) or not isinstance(timestamp, int) or timestamp <= 0:
        raise ContractError("source commit timestamp is invalid")


def _canonical_tag_payload(tag: str, commit: str, timestamp: int) -> bytes:
    _require_identity(tag, commit)
    _require_tagger_timestamp(timestamp)
    return (
        f"object {commit}\n"
        "type commit\n"
        f"tag {tag}\n"
        f"tagger {TAGGER_NAME} <{TAGGER_EMAIL}> {timestamp} +0000\n"
        "\n"
        f"RPi Plane Radar {tag}\n"
    ).encode()


def canonical_tag_object(tag: str, commit: str, timestamp: int) -> str:
    payload = _canonical_tag_payload(tag, commit, timestamp)
    header = f"tag {len(payload)}\0".encode()
    return hashlib.sha1(header + payload).hexdigest()


def _file_identity(path: pathlib.Path) -> dict[str, Any]:
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
            raise ContractError(f"release asset is not an exclusive regular file: {path.name}")
        digest = hashlib.sha256()
        with os.fdopen(descriptor, "rb") as source:
            descriptor = -1
            while True:
                chunk = source.read(1024 * 1024)
                if not chunk:
                    break
                digest.update(chunk)
        return {
            "path": path,
            "size": metadata.st_size,
            "digest": f"sha256:{digest.hexdigest()}",
        }
    finally:
        if descriptor >= 0:
            os.close(descriptor)


def _asset_inventory(release: dict[str, Any]) -> list[dict[str, Any]]:
    assets = release.get("assets")
    if not isinstance(assets, list):
        raise ContractError("release asset inventory is missing")
    inventory: list[dict[str, Any]] = []
    names: set[str] = set()
    total = 0
    for raw in assets:
        if not isinstance(raw, dict):
            raise ContractError("release asset entry is malformed")
        asset_id = raw.get("id")
        name = raw.get("name")
        size = raw.get("size")
        digest = raw.get("digest")
        if not isinstance(asset_id, int) or asset_id <= 0:
            raise ContractError("release asset ID is invalid")
        if not isinstance(name, str) or name not in REQUIRED_ASSETS or name in names:
            raise ContractError(f"release asset name is unexpected or duplicated: {name}")
        if not isinstance(size, int) or size <= 0 or size > MAX_ASSET_SIZE:
            raise ContractError(f"release asset size is invalid: {name}")
        if not isinstance(digest, str) or DIGEST.fullmatch(digest) is None:
            raise ContractError(f"release asset digest is missing or invalid: {name}")
        total += size
        if total > MAX_TOTAL_SIZE:
            raise ContractError("release assets exceed the total download limit")
        names.add(name)
        inventory.append(
            {"id": asset_id, "name": name, "size": size, "digest": digest}
        )
    if names != set(REQUIRED_ASSETS):
        missing = sorted(set(REQUIRED_ASSETS) - names)
        extra = sorted(names - set(REQUIRED_ASSETS))
        raise ContractError(f"release asset set differs: missing={missing} extra={extra}")
    return sorted(inventory, key=lambda item: item["name"])


def _asset_fingerprint(inventory: list[dict[str, Any]]) -> str:
    serialized = "".join(
        f"{item['id']}\t{item['name']}\t{item['size']}\t{item['digest']}\n"
        for item in inventory
    ).encode()
    return _sha256(serialized)


def _validate_draft_release(
    release: dict[str, Any],
    tag: str,
    commit: str,
    release_id: int,
    fingerprint: str | None = None,
) -> tuple[list[dict[str, Any]], str]:
    if release.get("id") != release_id:
        raise ContractError("draft release ID differs")
    if release.get("tag_name") != tag:
        raise ContractError("draft release tag name differs")
    if release.get("target_commitish") != commit:
        raise ContractError("draft release source commit differs")
    if release.get("draft") is not True or release.get("prerelease") is not False:
        raise ContractError("release is not the expected unpublished stable draft")
    inventory = _asset_inventory(release)
    actual = _asset_fingerprint(inventory)
    if fingerprint is not None and actual != fingerprint:
        raise ContractError("durable release asset identity fingerprint differs")
    return inventory, actual


def _validate_published_release(
    release: dict[str, Any],
    tag: str,
    commit: str,
    release_id: int,
    fingerprint: str,
) -> list[dict[str, Any]]:
    if (
        release.get("id") != release_id
        or release.get("tag_name") != tag
        or release.get("target_commitish") != commit
        or release.get("draft") is not False
        or release.get("prerelease") is not False
        or release.get("immutable") is not True
    ):
        raise ContractError(
            "published stable release identity, state, or immutability differs"
        )
    inventory = _asset_inventory(release)
    if _asset_fingerprint(inventory) != fingerprint:
        raise ContractError("published stable release assets changed")
    return inventory


def _local_assets(directory: pathlib.Path) -> dict[str, dict[str, Any]]:
    if not directory.is_dir() or directory.is_symlink():
        raise ContractError("release asset directory is missing or unsafe")
    entries = list(directory.iterdir())
    names = {entry.name for entry in entries}
    if names != set(REQUIRED_ASSETS):
        raise ContractError("local release asset set differs")
    result: dict[str, dict[str, Any]] = {}
    total = 0
    for entry in entries:
        if entry.is_symlink() or not entry.is_file():
            raise ContractError(f"local release asset is not a regular file: {entry.name}")
        identity = _file_identity(entry)
        size = identity["size"]
        if size <= 0 or size > MAX_ASSET_SIZE:
            raise ContractError(f"local release asset size is invalid: {entry.name}")
        total += size
        if total > MAX_TOTAL_SIZE:
            raise ContractError("local release assets exceed the total size limit")
        result[entry.name] = identity
    return result


def _validate_local_assets(
    directory: pathlib.Path, inventory: list[dict[str, Any]]
) -> dict[str, dict[str, Any]]:
    local = _local_assets(directory)
    for item in inventory:
        identity = local[item["name"]]
        if identity["size"] != item["size"] or identity["digest"] != item["digest"]:
            raise ContractError(f"local verified asset differs: {item['name']}")
    return local


def _record(
    tag: str,
    commit: str,
    release_id: int,
    inventory: list[dict[str, Any]],
    fingerprint: str,
    tag_object: str | None = None,
    tagger_timestamp: int | None = None,
) -> dict[str, Any]:
    record: dict[str, Any] = {
        "schema_version": 1 if tag_object is None else 2,
        "repository": REPOSITORY,
        "tag": tag,
        "commit": commit,
        "release_id": release_id,
        "asset_fingerprint": fingerprint,
        "assets": inventory,
    }
    if tag_object is not None:
        if tagger_timestamp is None:
            raise ContractError("publication record tagger timestamp is missing")
        record["tag_object"] = tag_object
        record["tagger_timestamp"] = tagger_timestamp
    return record


def _validate_record(record: dict[str, Any]) -> None:
    schema = record.get("schema_version")
    if schema not in (1, 2) or record.get("repository") != REPOSITORY:
        raise ContractError("verification record schema or repository differs")
    tag = record.get("tag")
    commit = record.get("commit")
    release_id = record.get("release_id")
    fingerprint = record.get("asset_fingerprint")
    if not isinstance(tag, str) or not isinstance(commit, str):
        raise ContractError("verification record identity is missing")
    _require_identity(tag, commit)
    if not isinstance(release_id, int) or release_id <= 0:
        raise ContractError("verification record release ID is invalid")
    if not isinstance(fingerprint, str) or not re.fullmatch(r"[0-9a-f]{64}", fingerprint):
        raise ContractError("verification record fingerprint is invalid")
    inventory = record.get("assets")
    if not isinstance(inventory, list) or _asset_fingerprint(inventory) != fingerprint:
        raise ContractError("verification record asset inventory differs")
    if schema == 1:
        if "tag_object" in record or "tagger_timestamp" in record:
            raise ContractError("verification record contains publication identity")
        return
    tag_object = record.get("tag_object")
    tagger_timestamp = record.get("tagger_timestamp")
    _require_tagger_timestamp(tagger_timestamp)
    if (
        not isinstance(tag_object, str)
        or re.fullmatch(r"[0-9a-f]{40}", tag_object) is None
        or tag_object != canonical_tag_object(tag, commit, tagger_timestamp)
    ):
        raise ContractError("publication record annotated tag object differs")


def create_stable_draft(
    backend: Any, tag: str, commit: str, assets_directory: pathlib.Path
) -> dict[str, Any]:
    _require_identity(tag, commit)
    if backend.remote_tag(tag) is not None:
        raise ContractError("stable tag already exists")
    local = _local_assets(assets_directory)
    release_id: int | None = None
    try:
        created = backend.create_release(tag, commit)
        release_id = created.get("id")
        if not isinstance(release_id, int) or release_id <= 0:
            raise ContractError("created draft release ID is invalid")
        if (
            created.get("tag_name") != tag
            or created.get("target_commitish") != commit
            or created.get("draft") is not True
            or created.get("prerelease") is not False
        ):
            raise ContractError("created release is not the requested stable draft")
        for name in sorted(local):
            identity = local[name]
            uploaded = backend.upload_asset(release_id, name, identity["path"])
            if (
                uploaded.get("name") != name
                or uploaded.get("size") != identity["size"]
                or uploaded.get("digest") != identity["digest"]
            ):
                raise ContractError(f"uploaded asset identity differs: {name}")
        release = backend.get_release(release_id)
        inventory, fingerprint = _validate_draft_release(
            release, tag, commit, release_id
        )
        if backend.remote_tag(tag) is not None:
            raise ContractError("stable tag appeared while draft assets were uploaded")
        return _record(tag, commit, release_id, inventory, fingerprint)
    except Exception:
        if release_id is not None:
            backend.delete_release(release_id)
        raise


def verify_stable_draft(
    backend: Any,
    tag: str,
    commit: str,
    release_id: int,
    fingerprint: str,
    downloads: pathlib.Path,
) -> dict[str, Any]:
    _require_identity(tag, commit)
    tagger_timestamp = backend.commit_timestamp(commit)
    _require_tagger_timestamp(tagger_timestamp)
    expected_tag = {
        "object": canonical_tag_object(tag, commit, tagger_timestamp),
        "commit": commit,
    }
    remote = backend.remote_tag(tag)
    release = backend.get_release(release_id)
    try:
        inventory, actual = _validate_draft_release(
            release, tag, commit, release_id, fingerprint
        )
        release_state = "draft"
    except ContractError:
        inventory = _validate_published_release(
            release, tag, commit, release_id, fingerprint
        )
        actual = fingerprint
        release_state = "published"
    if remote is None and release_state != "draft":
        raise ContractError("published stable release is missing its exact tag")
    if remote is not None and remote != expected_tag:
        raise ContractError("stable tag differs from the canonical retry object")
    if downloads.exists() or downloads.is_symlink():
        raise ContractError("stable verification download directory already exists")
    downloads.mkdir(mode=0o700, parents=True)
    try:
        remaining = MAX_TOTAL_SIZE
        for item in inventory:
            byte_limit = min(item["size"], MAX_ASSET_SIZE, remaining)
            if byte_limit <= 0:
                raise ContractError("stable release exhausted its aggregate download limit")
            path = downloads / item["name"]
            downloaded = backend.download_asset(item["id"], path, byte_limit)
            if (
                downloaded.get("size") != item["size"]
                or downloaded.get("digest") != item["digest"]
            ):
                raise ContractError(f"downloaded asset bytes differ: {item['name']}")
            remaining -= item["size"]
        _validate_local_assets(downloads, inventory)
    except Exception as error:
        for path in downloads.iterdir():
            if path.is_file() or path.is_symlink():
                path.unlink()
        downloads.rmdir()
        if isinstance(error, ContractError):
            raise
        raise ContractError("stable release asset download failed") from error
    return _record(tag, commit, release_id, inventory, actual)


def _publication_record(
    record: dict[str, Any],
    inventory: list[dict[str, Any]],
    tag_object: str,
    tagger_timestamp: int,
) -> dict[str, Any]:
    return _record(
        record["tag"],
        record["commit"],
        record["release_id"],
        inventory,
        record["asset_fingerprint"],
        tag_object,
        tagger_timestamp,
    )


def _release_state(
    release: dict[str, Any],
    tag: str,
    commit: str,
    release_id: int,
    fingerprint: str,
) -> tuple[str, list[dict[str, Any]]]:
    try:
        inventory, _ = _validate_draft_release(
            release, tag, commit, release_id, fingerprint
        )
        return "draft", inventory
    except ContractError as draft_error:
        try:
            return (
                "published",
                _validate_published_release(
                    release, tag, commit, release_id, fingerprint
                ),
            )
        except ContractError as published_error:
            raise ContractError(
                "stable release identity, assets, or state differs"
            ) from published_error


def confirm_published(backend: Any, publication_record: dict[str, Any]) -> dict[str, Any]:
    _validate_record(publication_record)
    if publication_record["schema_version"] != 2:
        raise ContractError("published confirmation requires a schema-2 record")
    tag = publication_record["tag"]
    commit = publication_record["commit"]
    expected_tag = {
        "object": publication_record["tag_object"],
        "commit": commit,
    }
    if backend.remote_tag(tag) != expected_tag:
        raise ContractError("published stable tag object or peeled commit differs")
    release = backend.get_release(publication_record["release_id"])
    _validate_published_release(
        release,
        tag,
        commit,
        publication_record["release_id"],
        publication_record["asset_fingerprint"],
    )
    return publication_record


def _compensate_rejected_publication(
    backend: Any,
    tag: str,
    commit: str,
    release_id: int,
    tag_object: str,
) -> str:
    expected_tag = {"object": tag_object, "commit": commit}
    last_release_error: Exception | None = None
    for _attempt in range(3):
        try:
            backend.delete_release(release_id)
        except Exception as error:
            last_release_error = error
        try:
            remaining_release = backend.get_release_optional(release_id)
        except Exception as error:
            last_release_error = error
            continue
        if remaining_release is None:
            break
    else:
        raise ContractError(
            "rejected stable publication could not be proven absent"
        ) from last_release_error

    last_tag_error: Exception | None = None
    try:
        remaining_tag = backend.remote_tag(tag)
    except Exception as error:
        raise ContractError(
            "rejected stable release was removed but tag state is ambiguous"
        ) from error
    if remaining_tag == expected_tag:
        try:
            backend.delete_remote_annotated_tag(tag, commit, tag_object)
        except Exception as error:
            last_tag_error = error
        try:
            remaining_tag = backend.remote_tag(tag)
        except Exception as error:
            raise ContractError(
                "rejected stable release was removed but tag cleanup is ambiguous"
            ) from error
    if remaining_tag == expected_tag:
        raise ContractError(
            "rejected stable release was removed but its exact tag remains"
        ) from last_tag_error

    local_error: Exception | None = None
    try:
        backend.discard_local_tag(tag, commit, tag_object)
    except Exception as error:
        local_error = error
    if remaining_tag is None:
        if local_error is None:
            return "the rejected release and its exact tag were removed"
        return (
            "the rejected release and its exact remote tag were removed; "
            "local tag cleanup failed"
        )
    return (
        "the rejected release was removed; a different remote tag was preserved"
    )


def _reject_published_mismatch(
    backend: Any,
    tag: str,
    commit: str,
    release_id: int,
    tag_object: str,
    cause: Exception,
) -> None:
    outcome = _compensate_rejected_publication(
        backend, tag, commit, release_id, tag_object
    )
    raise ContractError(
        f"stable publication did not preserve the accepted immutable bytes; {outcome}"
    ) from cause


def publish_verified_draft(
    backend: Any, verification_record: dict[str, Any], downloads: pathlib.Path
) -> dict[str, Any]:
    _validate_record(verification_record)
    tag = verification_record["tag"]
    commit = verification_record["commit"]
    release_id = verification_record["release_id"]
    fingerprint = verification_record["asset_fingerprint"]
    tagger_timestamp = backend.commit_timestamp(commit)
    _require_tagger_timestamp(tagger_timestamp)
    tag_object = canonical_tag_object(tag, commit, tagger_timestamp)
    expected_tag = {"object": tag_object, "commit": commit}
    remote = backend.remote_tag(tag)
    release = backend.get_release(release_id)
    try:
        state, inventory = _release_state(
            release, tag, commit, release_id, fingerprint
        )
    except ContractError as state_error:
        _reject_published_mismatch(
            backend,
            tag,
            commit,
            release_id,
            tag_object,
            state_error,
        )
    _validate_local_assets(downloads, inventory)
    if remote == expected_tag and state == "published":
        return _publication_record(
            verification_record, inventory, tag_object, tagger_timestamp
        )
    if remote is None and state == "draft":
        push_required = True
    elif remote == expected_tag and state == "draft":
        push_required = False
    else:
        raise ContractError(
            "stable tag and release are not an exact resumable publication state"
        )

    local_object = backend.ensure_local_annotated_tag(
        tag, commit, tagger_timestamp, tag_object
    )
    if local_object != tag_object:
        raise ContractError("local canonical annotated tag object differs")
    if push_required:
        push_error: Exception | None = None
        try:
            backend.push_annotated_tag(tag, tag_object)
        except Exception as error:
            push_error = error
        try:
            remote_after_push = backend.remote_tag(tag)
        except Exception as remote_error:
            raise ContractError(
                "remote tag state is ambiguous after push; no compensation was attempted"
            ) from remote_error
        if remote_after_push is None:
            cleanup_error: Exception | None = None
            try:
                backend.discard_local_tag(tag, commit, tag_object)
            except Exception as error:
                cleanup_error = error
            message = "stable tag push did not publish a remote ref"
            if cleanup_error is None:
                message += "; the exact local tag was removed for retry"
            else:
                message += "; exact local cleanup failed but remains retryable"
            raise ContractError(message) from (cleanup_error or push_error)
        if remote_after_push != expected_tag:
            raise ContractError(
                "remote stable tag differs after push; no destructive compensation "
                "was attempted"
            ) from push_error

    publish_error: Exception | None = None
    try:
        backend.publish_release(release_id)
    except Exception as error:
        publish_error = error
    try:
        observed_release = backend.get_release(release_id)
        observed_tag = backend.remote_tag(tag)
    except Exception as reread_error:
        _reject_published_mismatch(
            backend,
            tag,
            commit,
            release_id,
            tag_object,
            reread_error,
        )
    if observed_tag != expected_tag:
        _reject_published_mismatch(
            backend,
            tag,
            commit,
            release_id,
            tag_object,
            publish_error
            or ContractError("remote stable tag drifted after publication"),
        )
    try:
        observed_state, observed_inventory = _release_state(
            observed_release, tag, commit, release_id, fingerprint
        )
    except ContractError as state_error:
        _reject_published_mismatch(
            backend,
            tag,
            commit,
            release_id,
            tag_object,
            state_error,
        )
    if observed_state == "published":
        return _publication_record(
            verification_record,
            observed_inventory,
            tag_object,
            tagger_timestamp,
        )

    delete_error: Exception | None = None
    try:
        backend.delete_remote_annotated_tag(tag, commit, tag_object)
    except Exception as error:
        delete_error = error
    try:
        compensated_tag = backend.remote_tag(tag)
        compensated_release = backend.get_release(release_id)
    except Exception as reread_error:
        raise ContractError(
            "publication compensation is ambiguous after remote delete"
        ) from reread_error
    compensated_state, compensated_inventory = _release_state(
        compensated_release, tag, commit, release_id, fingerprint
    )
    if compensated_tag == expected_tag and compensated_state == "published":
        return _publication_record(
            verification_record,
            compensated_inventory,
            tag_object,
            tagger_timestamp,
        )
    if compensated_state != "draft":
        raise ContractError(
            "release state drifted during publication compensation"
        ) from (delete_error or publish_error)
    if compensated_tag == expected_tag:
        raise ContractError(
            "stable publication remains an exact tagged draft and can be retried"
        ) from (delete_error or publish_error)
    if compensated_tag is not None:
        raise ContractError(
            "remote stable tag drifted during publication compensation"
        ) from (delete_error or publish_error)

    cleanup_error: Exception | None = None
    try:
        backend.discard_local_tag(tag, commit, tag_object)
    except Exception as error:
        cleanup_error = error
    try:
        final_tag = backend.remote_tag(tag)
        final_release = backend.get_release(release_id)
    except Exception as reread_error:
        raise ContractError(
            "publication compensation is ambiguous after exact local cleanup"
        ) from reread_error
    final_state, final_inventory = _release_state(
        final_release, tag, commit, release_id, fingerprint
    )
    if final_tag == expected_tag and final_state == "published":
        return _publication_record(
            verification_record,
            final_inventory,
            tag_object,
            tagger_timestamp,
        )
    if final_tag is None and final_state == "draft":
        message = "stable publication was compensated to an exact retryable draft"
        if cleanup_error is not None:
            message += "; exact local cleanup failed"
        raise ContractError(message) from (cleanup_error or delete_error or publish_error)
    raise ContractError(
        "stable tag or release drifted after publication compensation"
    ) from (cleanup_error or delete_error or publish_error)


class GhGitBackend:
    def __init__(self) -> None:
        self.gh = os.environ.get("PLANERADAR_GH_BIN", "gh")
        self.git = os.environ.get("PLANERADAR_GIT_BIN", "git")

    def _gh_json(
        self, arguments: list[str], input_data: bytes | None = None
    ) -> dict[str, Any]:
        result = subprocess.run(
            [self.gh, "api", *arguments],
            input=input_data,
            stdout=subprocess.PIPE,
            stderr=None,
            check=True,
        )
        value = json.loads(result.stdout)
        if not isinstance(value, dict):
            raise ContractError("GitHub API returned a non-object")
        return value

    def remote_tag(self, tag: str) -> dict[str, str] | None:
        remote = subprocess.run(
            [
                self.git,
                "ls-remote",
                "--tags",
                "origin",
                f"refs/tags/{tag}",
                f"refs/tags/{tag}^{{}}",
            ],
            text=True,
            stdout=subprocess.PIPE,
            check=True,
        )
        lines = [line.split() for line in remote.stdout.splitlines() if line.strip()]
        if not lines:
            return None
        if any(len(line) != 2 for line in lines):
            raise ContractError("remote stable tag query returned malformed rows")
        values = {line[1]: line[0] for line in lines}
        if len(values) != len(lines):
            raise ContractError("remote stable tag query returned duplicate refs")
        tag_ref = f"refs/tags/{tag}"
        peeled_ref = f"{tag_ref}^{{}}"
        if set(values) != {tag_ref, peeled_ref}:
            raise ContractError("remote stable tag is not one exact annotated ref")
        for value in values.values():
            if re.fullmatch(r"[0-9a-f]{40}", value) is None:
                raise ContractError("remote stable tag object identity is invalid")
        return {"object": values[tag_ref], "commit": values[peeled_ref]}

    def commit_timestamp(self, commit: str) -> int:
        result = subprocess.run(
            [self.git, "show", "--no-patch", "--format=%ct", commit],
            text=True,
            stdout=subprocess.PIPE,
            check=True,
        )
        value = result.stdout.strip()
        if re.fullmatch(r"[1-9][0-9]*", value) is None:
            raise ContractError("source commit timestamp is invalid")
        timestamp = int(value)
        _require_tagger_timestamp(timestamp)
        return timestamp

    def create_release(self, tag: str, commit: str) -> dict[str, Any]:
        payload = json.dumps(
            {
                "tag_name": tag,
                "target_commitish": commit,
                "name": f"RPi Plane Radar {tag}",
                "draft": True,
                "prerelease": False,
            },
            separators=(",", ":"),
        ).encode()
        return self._gh_json(
            ["--method", "POST", f"repos/{REPOSITORY}/releases", "--input", "-"],
            payload,
        )

    def delete_release(self, release_id: int) -> None:
        subprocess.run(
            [
                self.gh,
                "api",
                "--method",
                "DELETE",
                f"repos/{REPOSITORY}/releases/{release_id}",
            ],
            check=True,
        )

    def upload_asset(
        self, release_id: int, name: str, path: pathlib.Path
    ) -> dict[str, Any]:
        endpoint = (
            f"https://uploads.github.com/repos/{REPOSITORY}/releases/"
            f"{release_id}/assets"
            f"?name={urllib.parse.quote(name, safe='')}"
        )
        return self._gh_json(
            [
                "--method",
                "POST",
                "-H",
                "Content-Type: application/octet-stream",
                endpoint,
                "--input",
                str(path),
            ],
        )

    def get_release(self, release_id: int) -> dict[str, Any]:
        return self._gh_json([f"repos/{REPOSITORY}/releases/{release_id}"])

    def get_release_optional(self, release_id: int) -> dict[str, Any] | None:
        result = subprocess.run(
            [
                self.gh,
                "api",
                f"repos/{REPOSITORY}/releases/{release_id}",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        if result.returncode == 0:
            value = json.loads(result.stdout)
            if not isinstance(value, dict):
                raise ContractError("GitHub API returned a non-object")
            return value
        if b"HTTP 404" in result.stderr:
            return None
        raise ContractError("GitHub API release lookup failed")

    def download_asset(
        self, asset_id: int, destination: pathlib.Path, byte_limit: int
    ) -> dict[str, Any]:
        if byte_limit <= 0 or byte_limit > MAX_ASSET_SIZE:
            raise ContractError("download byte ceiling is invalid")
        descriptor = os.open(
            destination,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0),
            0o600,
        )

        def apply_file_limit() -> None:
            resource.setrlimit(resource.RLIMIT_FSIZE, (byte_limit, byte_limit))

        try:
            with os.fdopen(descriptor, "wb") as output:
                descriptor = -1
                subprocess.run(
                    [
                        self.gh,
                        "api",
                        "-H",
                        "Accept: application/octet-stream",
                        f"repos/{REPOSITORY}/releases/assets/{asset_id}",
                    ],
                    stdout=output,
                    check=True,
                    preexec_fn=apply_file_limit,
                )
        finally:
            if descriptor >= 0:
                os.close(descriptor)
        return _file_identity(destination)

    def ensure_local_annotated_tag(
        self, tag: str, commit: str, tagger_timestamp: int, tag_object: str
    ) -> str:
        payload = _canonical_tag_payload(tag, commit, tagger_timestamp)
        if canonical_tag_object(tag, commit, tagger_timestamp) != tag_object:
            raise ContractError("requested canonical tag object differs")
        reference = f"refs/tags/{tag}"
        existing = subprocess.run(
            [self.git, "rev-parse", "--verify", "--quiet", reference],
            text=True,
            stdout=subprocess.PIPE,
            check=False,
        )
        if existing.returncode not in (0, 1):
            raise subprocess.CalledProcessError(existing.returncode, existing.args)
        if existing.returncode == 1:
            created = subprocess.run(
                [self.git, "mktag"],
                input=payload,
                stdout=subprocess.PIPE,
                check=True,
            ).stdout.decode().strip()
            if created != tag_object:
                raise ContractError("git created a different canonical tag object")
            subprocess.run(
                [
                    self.git,
                    "update-ref",
                    reference,
                    tag_object,
                    "0" * 40,
                ],
                check=True,
            )
        local_object = subprocess.run(
            [self.git, "rev-parse", reference],
            text=True,
            stdout=subprocess.PIPE,
            check=True,
        ).stdout.strip()
        local_commit = subprocess.run(
            [self.git, "rev-parse", f"{reference}^{{}}"],
            text=True,
            stdout=subprocess.PIPE,
            check=True,
        ).stdout.strip()
        if local_object != tag_object or local_commit != commit:
            raise ContractError("different local stable tag already exists")
        return tag_object

    def push_annotated_tag(self, tag: str, tag_object: str) -> None:
        local_object = subprocess.run(
            [self.git, "rev-parse", f"refs/tags/{tag}"],
            text=True,
            stdout=subprocess.PIPE,
            check=True,
        ).stdout.strip()
        if local_object != tag_object:
            raise ContractError("local annotated tag changed before push")
        subprocess.run(
            [self.git, "push", "origin", f"refs/tags/{tag}:refs/tags/{tag}"],
            check=True,
        )

    def delete_remote_annotated_tag(
        self, tag: str, commit: str, tag_object: str
    ) -> None:
        remote = self.remote_tag(tag)
        if remote != {"object": tag_object, "commit": commit}:
            raise ContractError("refusing to delete a different remote stable tag")
        subprocess.run(
            [
                self.git,
                "push",
                f"--force-with-lease=refs/tags/{tag}:{tag_object}",
                "origin",
                f":refs/tags/{tag}",
            ],
            check=True,
        )

    def discard_local_tag(self, tag: str, commit: str, tag_object: str) -> None:
        reference = f"refs/tags/{tag}"
        local = subprocess.run(
            [self.git, "rev-parse", "--verify", "--quiet", reference],
            text=True,
            stdout=subprocess.PIPE,
            check=False,
        )
        if local.returncode not in (0, 1):
            raise subprocess.CalledProcessError(local.returncode, local.args)
        if local.returncode == 1:
            return
        local_object = local.stdout.strip()
        local_commit = subprocess.run(
            [self.git, "rev-parse", f"{reference}^{{}}"],
            text=True,
            stdout=subprocess.PIPE,
            check=True,
        ).stdout.strip()
        if local_object != tag_object or local_commit != commit:
            raise ContractError("refusing to discard a different local stable tag")
        subprocess.run(
            [self.git, "update-ref", "-d", reference, tag_object],
            check=True,
        )

    def publish_release(self, release_id: int) -> dict[str, Any]:
        payload = b'{"draft":false,"prerelease":false}'
        return self._gh_json(
            [
                "--method",
                "PATCH",
                f"repos/{REPOSITORY}/releases/{release_id}",
                "--input",
                "-",
            ],
            payload,
        )


def _write_record(path: pathlib.Path, record: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "w") as output:
            fd = -1
            json.dump(record, output, sort_keys=True, separators=(",", ":"))
            output.write("\n")
        os.replace(temporary, path)
    except Exception:
        if fd >= 0:
            os.close(fd)
        pathlib.Path(temporary).unlink(missing_ok=True)
        raise


def _read_record(path: pathlib.Path) -> dict[str, Any]:
    value = json.loads(path.read_text())
    if not isinstance(value, dict):
        raise ContractError("verification record is not an object")
    _validate_record(value)
    return value


def main() -> int:
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    draft = commands.add_parser("draft")
    draft.add_argument("--tag", required=True)
    draft.add_argument("--commit", required=True)
    draft.add_argument("--assets", type=pathlib.Path, required=True)
    draft.add_argument("--record", type=pathlib.Path, required=True)
    verify = commands.add_parser("verify")
    verify.add_argument("--tag", required=True)
    verify.add_argument("--commit", required=True)
    verify.add_argument("--release-id", type=int, required=True)
    verify.add_argument("--asset-fingerprint", required=True)
    verify.add_argument("--downloads", type=pathlib.Path, required=True)
    verify.add_argument("--record", type=pathlib.Path, required=True)
    publish = commands.add_parser("publish")
    publish.add_argument("--record", type=pathlib.Path, required=True)
    publish.add_argument("--downloads", type=pathlib.Path, required=True)
    publish.add_argument("--result", type=pathlib.Path, required=True)
    confirm = commands.add_parser("confirm")
    confirm.add_argument("--record", type=pathlib.Path, required=True)
    args = parser.parse_args()
    backend = GhGitBackend()
    if args.command == "draft":
        record = create_stable_draft(backend, args.tag, args.commit, args.assets)
        _write_record(args.record, record)
    elif args.command == "verify":
        record = verify_stable_draft(
            backend,
            args.tag,
            args.commit,
            args.release_id,
            args.asset_fingerprint,
            args.downloads,
        )
        _write_record(args.record, record)
    elif args.command == "publish":
        record = publish_verified_draft(
            backend, _read_record(args.record), args.downloads
        )
        _write_record(args.result, record)
    else:
        record = confirm_published(backend, _read_record(args.record))
    print(json.dumps(record, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (ContractError, OSError, subprocess.CalledProcessError, json.JSONDecodeError) as error:
        print(f"stable release contract failed: {error}", file=sys.stderr)
        raise SystemExit(1)
