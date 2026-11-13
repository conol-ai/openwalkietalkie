# Builds, releases, and updates

OpenWalkieTalkie uses the same release model as
[OpenMicroKbd](https://github.com/conol-ai/openmicrokbd): a GitHub Release
contains the Mac app, signed Sparkle feeds, firmware, a release manifest,
checksums, and artifact attestations.

## CI

`.github/workflows/ci.yml` runs on pull requests and pushes to `main`, and
can be started manually. It tests the shared firmware and renderer for
LilyGo, Rev A, and Rev B; builds all three firmware configurations; tests
the Mac host and app; and compiles the Sparkle bridge. Rev A and Rev B
application images, temporary migration images, debug ELFs, and board-specific
recovery bootloaders are kept as workflow artifacts. Recovery uses the pinned
SiFli SDK; the build checks its SRAM vectors, board marker, and 64 KiB limit.

## Configure signed publishing

The GitHub environment **release** needs these secrets. Names match
OpenMicroKbd so existing Apple credentials can be provisioned the same way.

| Secret | Contents |
| --- | --- |
| `MACOS_DEVELOPER_ID_P12_BASE64` | Base64 Developer ID Application certificate and private key, exported as PKCS#12 |
| `MACOS_DEVELOPER_ID_P12_PASSWORD` | Nonempty password for that PKCS#12 export |
| `APPLE_API_KEY_ID` | App Store Connect key ID |
| `APPLE_API_ISSUER_ID` | App Store Connect issuer ID |
| `APPLE_API_PRIVATE_KEY_P8_BASE64` | Base64 private `.p8` key |
| `SPARKLE_ED25519_PRIVATE_KEY` | Base64 32-byte seed matching `packaging/macos/sparkle-public-key.txt` |

The OpenWalkieTalkie Sparkle key is stored locally under the Keychain
account `conol-ai.openmicrodial`. Keep a secure backup; changing the public
key alone prevents installed apps from accepting future updates. The
workflow derives the public key from its protected secret and verifies it
against the committed key before publishing. Never commit private keys.

Missing credentials, a mismatched Sparkle key, failed code signing, failed
notarization, or a failed test stops the release. An ad-hoc developer build
cannot become a published automatic update. Credentials are imported into
a temporary runner keychain and removed when the packaging step exits.

Configure environment reviewers and tag protection to match who may ship
releases. The `release` environment itself does not add reviewers by default.

## Ship a release

1. Bump `host/Cargo.toml`'s workspace version for the app and daemon.
   Bump the root `Cargo.toml` firmware version whenever firmware bytes or
   behavior change. Refresh the corresponding lockfiles. Host and firmware
   versions are independent, but both must be stable `MAJOR.MINOR.PATCH`.
2. Commit to `main` and wait for CI. Exercise the exact firmware on hardware,
   including interrupted USB transfer followed by recovery. CI cannot test
   the microphone, speaker, physical controls, or power loss.
3. Create and push `v<host-version>`. For host version `0.2.0`, use
   `git tag v0.2.0` followed by `git push origin v0.2.0`.

The release workflow verifies that the tagged commit belongs to `main`,
reruns CI, builds native Apple Silicon and Intel packages, signs and
notarizes the DMGs, signs and verifies both Sparkle feeds, creates the
manifest and SHA-256 checksums, and attests the assets. It uploads a draft
before making the complete release public. Published versions are immutable;
use a new tag to change a published release.

Windows packaging is not included: this project's companion app and voice
engines currently target macOS 13 or later.

## App updates

The packaged app links Sparkle 2.9.6. Developer ID builds enable automatic
checks on launch and every six hours and default to automatic downloads and
installation using Sparkle's standard UI and preferences. Both the feed and
update archive must be signed. **Check for updates** in the tray or
**Service → Updates** starts a manual check.

The app registers its bundled background agent on launch, separately from
the optional GUI login item. After an update, it compares the running
daemon's version and recorded launch path against its bundled daemon. A mismatch
requests `service.restart` with `when_idle:true`; active PTT, pending final
recognition, speech playback, and firmware operations defer the restart.
The daemon exits with code 75 and launchd runs the new bundled executable.
An unrelated development daemon is left alone.

Source and ad-hoc builds keep Sparkle installation disabled. They still
support firmware checks and updates through the running daemon.

## Factory full-chip images

Build the normal firmware and recovery assets for both boards first. Generate
a fresh flash table from the pinned SDK; never package a connected device's
flash dump. For example, for firmware 0.5.0:

```sh
source target/soc-build-env.sh
python3 "$SIFLI_SDK/tools/build/gen_ftab.py" \
  --ptab native/boards/openwalkietalkie/ptab.yaml \
  --output target/factory-ftab.bin \
  --bootloader-size 0x10000 --main-size 0x6E0000
python3 tools/factory-image.py \
  --firmware-dir dist/firmware \
  --ftab target/factory-ftab.bin --version 0.5.0 \
  --output dist/OpenWalkieTalkie-0.5.0-factory
python3 dist/OpenWalkieTalkie-0.5.0-factory/verify.py
```

Use a fresh output directory. The packager validates the flash table, board
markers, recovery vectors, normal application/ELF agreement and empty pairing
storage. It creates two full 16 MiB binaries, a component/hash manifest,
Chinese instructions, a standalone verifier, SHA256SUMS and a ZIP with its own
checksum. The README is generated from `docs/factory-flashing-zh.md`.
The release workflow generates the table with the pinned SDK and packages
the exact application and recovery artifacts distributed in that release.

Factory images start at **0x12000000 through the MCU UART downloader**, or
**offset 0 on a standalone SPI programmer**. They erase all external flash
contents, including device settings, bonds and any board-specific data. Use
them for blank-board provisioning; retain normal USB updates for shipped
devices. Qualify the board/flash combination on a first article before a
production run. This software packaging check does not replace a blank-board
programming and cold-boot test.

## Device firmware updates

With one deck connected by **USB**, open **Service** in the Mac app and click
**Check** on the **Deck firmware** row under **Updates**. A newer release also
produces a notification after the automatic check. Click **Review…**, check
the version, then **Install**. On a linked deck the service does not start yet:
the deck shows **Install firmware {version}? Press ENTER.** in its own
language, and only ENTER pressed on the deck itself starts the install. The
dialog says **Press ENTER on the deck to start the install.** meanwhile.
CANCEL, a minute without an answer, or any other answer leaves nothing
installed (**Not confirmed on the deck. Nothing was installed.**). After the
press, an install that finds the deck talking waits up to a minute for it to
finish (**Starting the install when the deck stops talking…**). A deck already
in recovery cannot show the card and installs at once. A deck that runs its
firmware but has not linked yet is refused with **Wait for the deck to link,
then install again.** The dialog then shows download, installation,
reconnection, and errors.

For a device with an old or unknown bootloader, the confirmation instead shows
**Bootloader update — power must stay connected** and an **I understand — update
device** button. It explains that losing power while replacing startup software
can prevent both boot and USB recovery and require hardware repair. USB-C must
stay connected and the computer must remain awake. The daemon also prevents idle
sleep for the duration of installation. This consent is enforced by the API,
not just the view; a client that omits it cannot initiate migration.

The daemon checks the stable manifest from this repository's GitHub Release
and the manifest bundled with the Mac app. It selects the newer valid firmware
release, preferring the bundled release when versions match. A complete,
checksum-verified bundle also works offline; its online check times out after
10 seconds. It validates product, board, protocol, flash layout, asset URL,
size, SHA-256, board marker, and vector table. A matching image bundled
inside the app is used when available; otherwise the release asset is
downloaded. The selected serial number is checked again before recovery and
used throughout reconnection. Multiple USB decks are refused during discovery.

Normal HID traffic pauses during installation and resumes on success or
failure. Transfers use the existing OWTB protocol with sector CRCs; the
bootloader commits the vector sector last. Protocol 2 exposes the two application
banks across the 16 MiB NOR and preserves both Bluetooth stores in the middle.
The updater validates FF padding over those stores and skips their sectors.
The release manifest uses protocol 2 and an image span of `0xFE0000` bytes;
128 KiB within that span is protected, leaving 15.75 MiB of application capacity.
Large images require recovery protocol 2. For Rev A/B devices still on protocol
1, an accepted migration verifies all three release images before writing:

1. Install the board's temporary `bootstrap` app, which fits the old lower bank.
2. Check its board identity and installed flash-table mapping. Back up the
   resident bootloader to the daemon's firmware cache under `bootloader-backups/`.
3. Stage the board's `recovery` image in SRAM, validate its CRC, write the
   bootloader slot, and verify the complete image by reading it back over USB.
4. Reboot into recovery and require protocol 2. Install the normal application,
   skipping both Bluetooth stores, and verify board, version, and removal of the
   temporary maintenance firmware.

Protocol-2 devices go straight to the normal application update. Small legacy
images/manifests remain supported. Neither path writes the flash table or
Bluetooth stores, and neither runs over Bluetooth. Bootloader replacement is
the sole exception to application-only flashing, with separate user consent.

If migration is refused before any application erase, the updater requests a
return to the existing application. If an installed temporary updater finds an
incompatible flash table, it refuses bootloader writes and reports that hardware
service is needed; the temporary app remains installed. A lost bootloader-write
reply or failed read-back never triggers an automatic reboot. Keep power
connected and retry: the temporary app is detected even at the target firmware
version, and the next attempt verifies or repairs the slot before rebooting.
After an interrupted final application transfer, retry from recovery.

If the deck is already in recovery or its older firmware cannot identify its
board, choose the physical revision printed on the board. Recovery mode
does not report its board, so this choice cannot be detected automatically.
After a cable interruption, reconnect, check again, and retry from recovery.
Success requires the same serial number to report the expected board and
version after reboot.

Daemon API calls are `firmware.status` (`observe`), `firmware.check`
(`admin`, optional `force`), and `firmware.install` (`admin`, with `board`,
`serial`, and `version` copied from the checked release, plus
`allow_bootloader_update:true` only after accepting the warning).
`firmware.status` includes `bootloader_warning`; its device includes optional
`recovery_protocol` and a `maintenance` flag. On a linked deck,
`firmware.install` answers at once with the phase `confirming` and waits for
ENTER on the deck, as above; a second install meanwhile is refused as busy.
Every phase of an operation (`checking`, `confirming`, `downloading`,
`installing`, `reconnecting`, `bootloader`) is busy and prevents service
restarts. Installation is always explicit and, unless the deck is already in
recovery, confirmed on the deck; scheduled checks never flash a device.

### Hardware validation status

The 0.4.0 migration is implemented and tested in software. **A shipped Rev B
board was unavailable; its end-to-end hardware migration is not validated.**
Keep this limitation explicit in the 0.4.0 release notes. To complete validation,
exercise the exact release artifacts on a shipped Rev B:
warning acceptance/cancellation, normal migration, pairing preservation, USB
reconnection, interrupted application transfers, and retry while the temporary
app is installed. Verify that maintenance commands are absent afterward. Do not
cut power during the single-slot bootloader write without a UART recovery setup.
Software tests cannot establish recovery from physical power loss in that window.

The new recovery image also drives the Rev A/B display. Before release, check
bulk enumeration and transfer on the supported desktop OS, HID fallback with
an older host, sparse-sector erasure over stale data, and retry after a failed
or disconnected bulk transfer. Confirm that both Bluetooth stores are unchanged.
Existing protocol-2 bootloaders retain HID support and are not automatically
replaced solely for bulk capability; bootloader writes retain the explicit warning.
For the display, check upright recovery/missing-firmware instructions,
percentage progress, completion,
CRC/flash errors and USB reconnect/retry on both revisions. Confirm normal boots
hand display initialization directly to the app. The renderer and progress
accounting are covered by software tests; panel timing/appearance and physical
USB interruption remain hardware checks. The on-device screen starts once the
new bootloader runs; the existing desktop warning covers bootloader replacement.

## Local validation

```sh
python3 -m unittest discover -s tools/tests -p 'test_release.py'
# First load the pinned SiFli SDK environment (SIFLI_SDK, RTT_EXEC_PATH, PYTHONPATH).
bash tools/build-firmware-release.sh hw-a dist/firmware
bash tools/build-firmware-release.sh hw-b dist/firmware
tools/package-macos.sh dist/macos
```

The last command produces an ad-hoc package unless signing credentials are
provided. `REQUIRE_SIGNING=1` requires both code signing and notarization.
For an internal 0.4.0 test DMG, set `OWT_FIRMWARE_DIR` to the directory containing
the six board images and `firmware-manifest.json`, and set `OWT_RELEASE_README`
to `docs/releases/0.4.0-testing.zh-CN.md` to include the Chinese testing guide.
The guide describes a signed but unnotarized internal package; update that
disclosure when producing a notarized release.
`release-manifest.json` generated by packaging describes that package;
the release workflow combines all verified packages and firmware into the
public updater manifest using `tools/release.py manifest`.
