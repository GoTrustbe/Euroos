# EuroUpdate: how an installed EuroOS updates itself

An installed EuroOS keeps itself current the way a phone does: it checks a signed
channel, downloads the new kernel over its own TLS stack, writes it to the inactive
A/B slot, and boots it with automatic rollback. No image download, no USB stick.
This document is the operator's view: what happens on the machine, what happens on
the server, and how the signing keys are managed without a second machine.

## On the machine

| Step | What | Where |
|---|---|---|
| Boot | The loader reads `\slot_config` from the ESP and boots the selected slot **from its GPT partition** (`EuroSlot-A`/`-B`), after checking the slot header: magic `EUROSLT2`, length, SHA-256 and an Ed25519 signature against the embedded keys. A slot that fails falls back to the other slot, then to the ESP file. | `loader/src/main.rs` |
| Boot + 90 s, then every 6 h | The kernel starts a **background check**: manifest, manifest signature, image, image signature, each fetched cooperatively (one non-blocking slice per desktop iteration, so the desktop keeps drawing while ~50 MB come in). A failed or unreachable check retries after 30 min. | `kernel/src/update.rs` (`maybe_check`, `step`) |
| Verify | Manifest signature (fail closed, nothing else is fetched on failure), strictly newer version than the running `EUROOS_BUILD_VERSION`, not expired, not below the anti-rollback watermark (`/etc/euroupdate.seen`), sane image path, SHA-256 of the image equal to the pinned hash, Ed25519 over the image. | `evaluate_manifest`, `finish_image` |
| Stage | Image sectors are written to the inactive slot first, the header last, so a torn write never yields a header pointing at garbage. Then `slot_config` marks the slot *Trying*. | `write_image_to_slot`, `stage_verified_image` |
| Apply | Reboot. The loader boots the new slot; the kernel confirms it **GOOD** once the desktop is up. If it never confirms within the bounded number of tries, the loader goes back to the old slot. | `mark_boot_good` |

Works on whatever the root disk is: virtio, NVMe or AHCI/SATA (`rootblk::boot_*`
routes the slot and ESP I/O to the disk that was booted; a RAM-root session has no
boot disk and never writes slot state to a data disk). The only blocking call left
in the background job is the DNS lookup (UDP, its own short timeout); the TCP
handshake, TLS and the download are all step-driven.

### Policy

`euroupdate policy <ask|auto|manual>` (root), stored in `/etc/euroupdate.conf`:

- **ask** (default): the update is verified and staged, a notification asks for a restart.
- **auto**: staged, then the machine restarts 2 minutes later.
- **manual**: only the manifest is checked; a notification says a version is available. `euroupdate check` installs it.

`euroupdate status` shows the running version, policy, server, trusted key
fingerprints, anti-rollback watermark, last result and what is staged.

## On the server (euro-os.eu)

```
scripts/server/publish-release.sh          # build today, sign, publish download/ + update/ + live-try image
```

`release-web.sh` produces the download images and, from the same kernel,
`update/channel/stable.json(.sig)` and `update/images/euroos-<version>.efi(.sig)`
via `toolchain/update-server/make-channel.py`. The manifest carries `version`,
`image`, `sha256`, `size`, `built` and `expires` (120 days; an expired manifest is
refused, so a captured old manifest cannot be replayed forever).

## Keys

Every installed EuroOS trusts exactly two Ed25519 public keys, embedded at build
time in the kernel and the loader (`toolchain/eupkg/keys/`):

| Key | Signs | Private half |
|---|---|---|
| **daily** `dev.pub` | every ordinary release | `dev.key`, root-only on the build server, git-ignored |
| **rotation** `rotation.pub` | nothing day to day; one release that replaces the daily key | `/root/euroos-keys/rotation.key.age`, encrypted with a passphrase that lives only in the operator's password manager |

The point: a compromised build server leaks at most the daily key. It cannot
produce a rotation-signed release, because the rotation key's passphrase is not on
the server. Recovery does not need a second machine:

```
scripts/server/rotate-signing-key.sh       # asks for the passphrase
```

This decrypts the rotation key into RAM (`/dev/shm`, shredded on exit), retires
the old daily key to `/root/euroos-keys/retired/`, generates a new one, builds a
kernel that embeds the new `dev.pub`, and publishes that one release signed with
the rotation key. Installed systems accept it (they trust `rotation.pub`), boot it,
and from then on accept only the new daily key. Commit the new `dev.pub` afterwards.

The rotation key itself was created with `toolchain/update-server/gen-rotation-key.py`,
which never writes the seed to disk in plaintext: generate in RAM, `age -p`, write
the ciphertext and a random 8-word passphrase (`/root/euroos-rotation-passphrase.txt`,
to be moved to the password manager and deleted). If the rotation key were ever
lost, a new one can only reach installed systems through a daily-key-signed
release, so keep the passphrase in two places.

## What the loader will and will not boot

The loader tries, in order: the chosen slot partition, the other slot partition
(and rolls `slot_config` back to it, so the broken slot is not confirmed good by
mistake), the ESP file of the chosen slot, the ESP file of slot A. Each candidate
is verified first: a slot by its header (length, SHA-256) and Ed25519 signature,
an ESP file by the detached `.sig` next to it. `build.sh` signs the kernel and
ships both files; the installer writes the signature into the slot A header, so a
fresh install boots through the verified partition path from the first boot on.
Nothing unsigned is ever handed to `LoadImage`.

Threat model, honestly: without UEFI Secure Boot the loader itself
(`BOOTX64.EFI`) is an unsigned file on the ESP, so these checks cannot stop an
attacker who can write the disk offline; they protect the over-the-air path and
detect corruption or a torn write. With Secure Boot enabled the firmware's policy
covers the loader. The slot signature is over the image bytes only: an older,
validly signed kernel written into a slot by such an attacker would boot; the
anti-rollback watermark applies to the update channel, not to the disk. On media
with a block size other than 512 bytes the loader skips the slot partitions and
uses the (signed) ESP files.

## Rescue channel (after a key rotation)

A system that is offline while the daily key is rotated later sees a stable
manifest signed by a key it does not trust. It then fetches
`channel/rescue.json`: the rotation-signed release, kept published (the publish
script never deletes `rescue.json` or `images/rescue.efi`), whose kernel carries
the new daily key. After that one update the stable channel verifies again.
`rotate-signing-key.sh` publishes the rescue channel as part of the rotation.

## Testing

`scripts/ota-test.py` drives the whole chain under QEMU (TCG): install the v1
image to a blank disk, boot the installed disk with networking against the live
channel and wait for `[euroupdate] staged version`, reboot and wait for
`[euroupdate] slot B confirmed GOOD` (`ota-test.py <install|update|reboot|all> [virtio|ahci|nvme]`). Run it with `virtio`, `ahci` or `nvme` as
the target bus. Proof logs live in `docs/proof/ota-<date>/`.
