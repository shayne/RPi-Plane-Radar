# Contributing

Plane Radar supports one hardware and OS configuration today. A change that
works on another Pi is welcome evidence, but it is not a new support promise
until the installer, driver, recovery path, and physical display all agree.

## Start with the boundary

Application, control-plane, and kernel-driver changes have different failure
modes:

- renderer, network, web, settings, and target installer work belongs here;
- Mac orchestration, releases, SSH transport, and lifecycle work belongs in
  `crates/planeradarctl`; and
- panel, GPIO, Device Tree, touch binding, DKMS, and tryboot work belongs in
  [hyperpixel2r-kms](https://github.com/shayne/hyperpixel2r-kms).

Do not vendor the driver or add it as a submodule.

## Develop and test

```sh
git clone https://github.com/shayne/RPi-Plane-Radar.git
cd RPi-Plane-Radar
mise install
mise run verify
mise run docs-check
```

Add a failing regression test before the implementation. Keep network tests
hermetic, use synthetic target identities, and never copy live settings,
serials, coordinates, `.env`, or debug frames into fixtures.

For documentation, keep commands copyable and check facts against the current
CLI:

```sh
mise exec -- cargo run --locked -p planeradarctl -- --help
```

## Pull requests

Keep a change narrow enough that its failure boundary is visible. Explain:

- what behavior changes;
- which state or owner is involved;
- how interruption, retry, and rollback behave;
- which commands prove the result; and
- whether physical hardware was tested.

Run `mise run verify` and `git diff --check` before opening the pull request.
Do not include target addresses, private logs, screenshots with live callsigns,
or GitHub credentials.

AI-assisted code is accepted, but assistance is not review. Disclose material
use, preserve the project's Codex co-author convention where applicable, and
take responsibility for understanding and testing the submitted change.

## Licenses and provenance

Contributions to this repository are licensed under the
[MIT License](LICENSE). Preserve the bundled font notices and any third-party
attribution. GPL kernel work belongs in the driver repository under its
GPL-2.0-only terms.

