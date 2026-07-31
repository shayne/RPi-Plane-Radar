# Release Plane Radar

Plane Radar has three release gates. A release candidate proves the public
build and installer. A stable draft proves the final bytes on real hardware.
Promotion makes those same bytes public without building or uploading them
again.

## Before releasing

Start from a clean public `main` with green CI. The checked-in driver lock must
name a published stable driver, not a driver release candidate. GitHub release
immutability must remain enabled for both the app and driver repositories:

```sh
mise install
mise run verify
mise run driver:sync
```

Do not release from a private branch or a local-only commit. Both release
workflows bind their artifacts and attestations to the exact public `main`
commit that dispatched them.

## Publish a release candidate

Choose the next unused candidate tag and dispatch the candidate workflow:

```sh
gh workflow run release.yml \
  --repo shayne/RPi-Plane-Radar \
  --ref main \
  -f tag=v0.1.0-rc.N \
  -f source_ref=main
```

The workflow builds Linux ARM64 and both native macOS control binaries,
packages the release twice, validates its checksums and SPDX data, creates the
annotated tag, attests the release subjects, and publishes the verified
prerelease. A successful run is immediately installable with
`--version 0.1.0-rc.N`.

Download and inspect the public candidate before touching a Pi:

```sh
gh release download v0.1.0-rc.N \
  --repo shayne/RPi-Plane-Radar \
  --dir dist/accepted-rc
(cd dist/accepted-rc && shasum -a 256 -c SHA256SUMS)
gh release verify v0.1.0-rc.N --repo shayne/RPi-Plane-Radar
```

Install that public candidate through the documented Mac controller. Run
`doctor`, capture a screenshot, and verify display, touch, reboots, upgrade,
rollback, and uninstall recovery on the supported Pi and panel.

## Create a stable draft

Stable publication starts only after the release-candidate source and the
stable driver have passed hardware acceptance. Dispatch:

```sh
gh workflow run stable-draft.yml \
  --repo shayne/RPi-Plane-Radar \
  --ref main \
  -f tag=v0.1.0 \
  -f source_ref=main
```

This workflow runs the complete source verification again, builds every
platform artifact, proves reproducibility, validates metadata, attests the
subjects, and creates an unpublished GitHub draft. It deliberately does not
create `v0.1.0`.

The workflow summary records two values:

- the numeric draft release ID;
- the SHA-256 fingerprint of every asset ID, name, size, and server digest.

Record those values with the exact source commit. Download the draft by ID,
verify it, and install those exact files:

```sh
GH_TOKEN="$(gh auth token)" python3 scripts/stable_release.py verify \
  --tag v0.1.0 \
  --commit FULL_ACCEPTED_COMMIT \
  --release-id ACCEPTED_RELEASE_ID \
  --asset-fingerprint ACCEPTED_ASSET_FINGERPRINT \
  --downloads dist/accepted-v0.1.0 \
  --record dist/accepted-v0.1.0.json
chmod -R a-w dist/accepted-v0.1.0
mise run upgrade -- user@host --release-dir "$PWD/dist/accepted-v0.1.0"
```

Keep the download directory read-only during acceptance. A changed asset
means a new source fix and release candidate; do not replace an accepted draft
asset.

## Promote the accepted draft

After the draft itself passes automated and physical acceptance, dispatch:

```sh
gh workflow run stable-promote.yml \
  --repo shayne/RPi-Plane-Radar \
  --ref main \
  -f tag=v0.1.0 \
  -f source_commit=FULL_ACCEPTED_COMMIT \
  -f release_id=ACCEPTED_RELEASE_ID \
  -f asset_fingerprint=ACCEPTED_ASSET_FINGERPRINT
```

The promotion workflow downloads only the recorded draft assets. It checks
their IDs, names, sizes, digests, checksums, schema, SPDX document, archive
safety, source identity, and attestations. Only then does it create the
canonical annotated tag and publish that same draft. There is no build or
upload step in promotion. The final gate also requires GitHub to report the
release as immutable, verifies GitHub's release attestation, and verifies every
downloaded asset against that attestation.

If assets or state drift during publication, promotion deletes only the
recorded release ID and its own exact annotated tag, then fails. A different
tag is never deleted or force-updated. GitHub can permanently reserve an
immutable tag name even after a rejected release is deleted, so recover with
the next patch version rather than trying to reuse it. Any other state that
cannot be proven safe stops for manual inspection.

## Verify the public stable release

Finish from a fresh clone and a normal public download:

```sh
gh release verify v0.1.0 --repo shayne/RPi-Plane-Radar
gh release download v0.1.0 \
  --repo shayne/RPi-Plane-Radar \
  --dir dist/public-v0.1.0
(cd dist/public-v0.1.0 && shasum -a 256 -c SHA256SUMS)
mise run upgrade -- user@host --version 0.1.0
mise run doctor -- user@host
```

The installed application commit, application hash, driver version, driver
commit, and driver manifest hash must match the published release manifest and
the checked-in driver lock.
