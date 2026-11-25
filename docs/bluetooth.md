# Bluetooth (Settings › Network › Bluetooth)

Version 0.8.x controls the Kindle's own Bluetooth stack (`com.lab126.btfd`, through lipc). It pairs and connects
classic audio devices (headphones, speakers). Keyboards are not supported (see Gaps below). Sound to headphones uses the
Kindle's own player (`mplay`), as the Test sound button does.

## Where the code is

- `tools/crates/kl-core/src/bluetooth.rs`: `BdAddr` (its text is private: Debug and Display print `<addr>`), `Device`,
  `BtInfo`, the parsers for the stack's lists, `merge` (connected, then paired, then nearby audio by signal), and tests.
- `tools/crates/primitive-device/src/bluetooth.rs`: the lipc calls. Each one has a time limit. `pair` waits for the
  stack's events with a deadline and kills its `lipc-wait-event` when done.
- `tools/crates/kl-core/src/ui.rs` and `layout.rs`: the page, the device and confirmation screens, the quick tile and the
  status-bar icon.
- `crates/kl-launcher/src/main.rs`: `start_bt_*` threads. Every stack call runs on a thread, and the result comes back as
  an event (`Ev::Bluetooth`, `Ev::BluetoothNotice`, `Ev::BluetoothConfirm`).

## Verified on the Kindle (Oasis 3, firmware 5.18.2)

- On: `lipc-set-prop com.lab126.btfd BTenable 1:1`. `BTstate` reads 1 about two seconds later.
- Off: `initctl restart btfd`. `BTenable 0`, `0:0` and `BTflightMode 1` do not turn the radio off. `BTenable 0:1`
  works only when no scan is running, so the launcher uses the restart.
- Scan: `lipc-set-prop -i com.lab126.btfd triggerBTscan 1` (an integer). Without `-i` the property is missing. The
  scan stops by itself; `triggerBTscan 0` is refused.
- A scan with no device in pairing mode leaves `ListDiscovered` empty.
- `lipc-hash-prop -n` reads the lists: the stack's own fields are `bd_name`, `bd_address`, `class_of_device`, `rssi`,
  `is_paired`, `is_connected`, `is_discovered` (from `lipc-probe`).

## Not verified yet (confirm on the device with a headset in pairing mode)

- The list format is confirmed for paired devices (the AirPods Pro capture, 2026-11-25: `Hash Index: 0` blocks, numbers
  in parentheses, class names such as "Audio/Video"). A nearby classic device has not been captured yet.
- The payload of `btConfirmationEvent` (the code shown, if any). The screen shows whatever text arrives.
- The meaning of the `Bond_Result` value. The code treats a first value of `0` as success.
- Whether the radio stays on across sleep. The launcher never enables Bluetooth while the Kindle sleeps.

## Behaviour

- The launcher never turns Bluetooth on by itself. A scan starts when the Bluetooth page opens (while the radio is on).
- Device addresses go only to lipc. Notices, logs, screenshots and tests use names or `<addr>`.
- Pairing: Pair → the stack may ask for a confirmation → the code shows → Pair (or Cancel) answers the stack.
- A paired device that is gone from the lists (forgotten) returns the screen to the Bluetooth page.

## Audio

Sound reaches connected headphones through the stock audio manager: the launcher runs `mplay <file.wav>` (a WAV or raw
PCM file), and the stock stack sends it over A2DP. Verified 2026-11-25 with AirPods Pro: a short test tone and a 10 s
melody were heard. The Kindle has no sound card, so this is the only route. The own A2DP protocol was tried and
stopped: the AirPods close the SDP and AVDTP channels after our first data request. That work is archived, not
shipped.

## Gaps and plan

These are known and deliberately not in 0.8.x. Each needs an owner decision before work starts.

### 1. Devices that only advertise over Bluetooth Low Energy (LE)

- Symptom: a headset in pairing mode may not appear on the Bluetooth page. Verified 2026-11-25 with the UGREEN HiTune
  S3 Air: our LE scan saw it close by, but the stock stack's list (classic discovery) stayed empty for it.
- Why: the stock stack lists only what its classic discovery finds. Its pairing also needs a device record, which an
  LE-only device does not get (`Bond` answers result 7, "bt_device is null"; `ensureBTconnection` fails).
- Options:
  a. LE listing only: scan LE with our own HCI code, which takes the controller from the stock stack for about 40 s, and
     show the names on the page. Display only: pairing still needs the stock stack or our own LE pairing.
  b. LE pairing (SMP) and LE connection in our own stack, then audio over LE or classic. Large; several weeks.
- Plan: decide between a and b. Option a is the next step if the owner wants the UGREEN listed.

### 2. Keyboards (HID)

- Symptom: no keyboard can be paired or used. The stock stack has no HID profile (`bsa_server` has A2DP, BLE/GATT and
  DM only).
- Options:
  a. BLE HID over GATT through the stock stack. Needs a GATT server with the HID service; the stack's API for it is not
     explored yet.
  b. Our own stack on the UART with the Broadcom firmware. Large. The reference project (`kindle-hid-passthrough`, GPL-3.0)
     does this for the Oasis 3 with a replacement stack: read only, no code copied without an owner decision.
- Input to Linux: a virtual keyboard needs the `uhid` kernel module. The reference's build for this firmware (zelda,
  build 443455) loads and opens `/dev/uhid` on our Kindle (checked 2026-11-25). It is GPL-2.0 kernel code, so it stays
  out of the repos until the owner decides on the licence.
- Plan: decide between a and b, then the uhid licence decision. Keyboards are not in the next release.

### 3. Acceptance with the owner (before a release)

On the Kindle, with the owner present: turn on from the page and the tile; scan; pair; connect; the status-bar icon and
the headphones icon; disconnect; forget; sleep and wake while connected; Wi-Fi still works; launcher memory (about 22 MB)
and battery over a session.
