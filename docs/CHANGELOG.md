# Changelog

Each device build gets a new patch version; a new phase starts a new minor version. Releases are the versions
with a `v*` tag.

## 0.8.5

- Quick settings has a Sound row while headphones are connected: − and +, or a tap on the bar, change the Kindle's sound level (eight levels). The Music app uses the same level.

## 0.8.4

- The status bar and quick settings drop the headphones icon when they leave (put in their case, or switched off). The connected device is read every five seconds, not only when the Bluetooth page is opened.

## 0.8.3

- While Bluetooth audio is connected, the status bar and quick settings show headphones beside the Bluetooth icon.
- The own Bluetooth audio work (A2DP) is removed; audio uses the Kindle's own player (mplay), as the test sound does.

## 0.8.2

- Bluetooth: a Test sound button on the Bluetooth page plays a short tone through the connected headphones. It asks you to connect headphones first when none are connected.

## 0.8.1

- Bluetooth reads the Kindle's own device lists as the stack writes them: paired devices show their names, and their class (audio, for headphones and speakers) is recognised.

## 0.8.0

- Bluetooth: Settings › Network › Bluetooth turns the Kindle's Bluetooth on and off, finds headphones and speakers, and pairs, connects, disconnects and forgets them. Keyboards are not supported yet.
- Bluetooth is a quick-settings tile. While it is on, a Bluetooth icon shows in the status bar beside Wi-Fi. The launcher never turns Bluetooth on by itself, and a scan runs when the Bluetooth page opens.
- The SSH server shows as a prompt icon beside the Wi-Fi icon, in the status bar and in quick settings, while it runs. It replaces the SSH text label in the status bar.

## 0.7.0

- Settings is grouped as on iPhone: Network, Display (with Wallpaper and Sleep), Battery, Applications, and Device.
- Battery: the 24-hour chart is only battery. A usage card below it shows the screen-on time, the number of wakes, and the most used apps for the last seven days (an estimate, not a battery percentage). Readings are saved every five minutes.
- Sleep: "Sleep after" sets how long the screen stays on, from 15 seconds to 10 minutes or never. It is also a quick-settings tile.
- Hold both page-turn buttons for two seconds to restart the launcher (after a confirmation). The app you have open closes.
- The launcher no longer shows a startup picture; the stock boot screen stays.

## 0.6.22

- The device name page says Save, and its top button says Cancel (it leaves without saving). An unset device name shows "Tap to set".

## 0.6.21

- Settings › Device has a Device name row: type the name of this Kindle on the keyboard (1 to 32 letters, digits or spaces).
- Device information shows the device name, the series number, the Wi-Fi MAC address, the IP address and the screen panel. The serial number is not shown.

## 0.6.20

- The Kindle model is detected from the device (its platform codename). Settings › Device shows the model, and where it cannot be detected, "Kindle model" lists the supported Kindles to choose from. Any Kindle that has the processor the apps need (armhf) can install; the launcher says when the model has not been tested.
- The Store lists only apps with a package for this Kindle's processor, and says when an app is not tested on this Kindle yet. The launcher writes the device details (no serial number) for the Store.

## 0.6.19

- The Store's updates badge is recounted when the Store closes, so an update installed or refreshed in the Store is not still counted on the Store icon.

## 0.6.18

- Settings has its gear as an icon like the other apps', in light and dark mode.
- The file transfer answers a wrong code without a connection reset, so the transfer test passes on Linux (the 0.6.17 release run failed there, and was not published).
- The launcher's shared kit (tools) moved to the commit that fixes the test casts, so the launcher's clippy check passes in CI.

## 0.5.9

- Uninstall apps directly from Settings › Apps › Installed apps, including offline. The list also opens from Settings › Storage.
- Refresh Home and the installed list after removal. App data outside the installation record is preserved.
- Save Wi-Fi profiles using the current Kindle firmware's saved-network commands and refresh the Launcher boot copy after updates.

## 0.5.5

- Preview Home, Sleep, and Startup wallpapers before applying them. Each screen saves its own opacity.
- Extend the Home wallpaper behind the status bar and page indicator while keeping controls readable.

## 0.5.4

- Use the PrimitiveOS organization’s Store catalog for bootstrap and update checks.

## 0.5.3

- Add Display › Wallpaper with separate PNG choices for the Home Screen, Sleep Screen, and automatic Launcher startup.

## 0.5.2

- Scroll vertically through Settings and App visibility. Back from a setting returns to the same position in Settings.

## 0.5.1

- Add Date and time with an on-demand Wi-Fi clock sync and the Kindle's current time zone.

## 0.5.0

- Add Battery with charge state, local 24-hour history, and an opt-in Low power mode.
- Share direct front-light and warmth adjustment between Display and Quick Settings.
- Add Accessibility for larger text, higher contrast, and opening animation.
- Add Storage and expand Device information with the firmware, device profile, and USB state.
- Manage home-screen visibility and grid size from Apps. Open Store's installed list from Apps or Storage.

## 0.4.10

- Keep an uninstalled app's saved grid preferences without showing a missing-app warning. Reinstalling the app restores its preferences.

## 0.4.9

- Find and install Store on Kindle firmware where the optional signing key is absent and the signing-policy directory is group-writable.

## 0.4.8

- Check the Launcher files by content on startup so archive timestamps cannot leave an older launcher running.

## 0.4.7

- Use the approved Store and native app icons in the Launcher grid. Apps can still supply their own icons.

## 0.4.6

- Show one Store tile, using the standalone app when installed and offering installation when needed.

## 0.4.5

- Add standalone Store bootstrap and expose Store updates and preferences in Launcher.
- Protect Launcher updates with a startup trial and automatic rollback.
- Add Wi-Fi file transfer and storage lease support for the Store and native apps.

## 0.4.0

- Primitive Store: a Store tile on the home grid. Its screens follow the App Store's layout: Today (a featured app and
  the next apps), Apps, Updates, and an app's page (icon, action, information, screenshots, About, and what the app
  changes). A floating tab bar at the bottom.
- The catalog is downloaded with Refresh, over Wi-Fi, from the catalog's published site. A failed app keeps its last copy.
- Get installs an app from the store: the package for this Kindle is downloaded, checked against the catalog (size,
  SHA-256, id, version, architecture and requirements), and installed. The library shows it after the install.
- Remove on an installed app's page (tap twice). Update installs the newer version over the old one.
- An install interrupted by a power cut is finished or undone at the next start.
- Not yet: the Search tab, the install sheet with its warnings, and the store's daily update check.

## 0.3.21

- Licence: the code (including Primitive Store) is under the PolyForm Noncommercial License 1.0.0, and the
  documentation and pictures under CC BY-NC-SA 4.0. Releases up to v0.3.20 stay under AGPL-3.0. The release package
  carries both licence files.

## 0.3.20

- Power off shows "Powered Off" on the screen, and the screen keeps that picture once the Kindle is off.

## 0.3.19

- Power off shows a message on the screen before the Kindle turns off.

## 0.3.18

- Installing by USB is complete: opening Launcher once sets up the boot start by itself (the copy on `/var/local` and the
  boot job), so no computer or SSH is needed. Each later open checks the copy and the job and updates them if the
  launcher changed. Uninstalling stops the boot start once the `launcher` folder is gone.
- Power off turns the Kindle off without the stock "restarting" cover: the launcher runs `halt -f -p`, which skips the
  stock shutdown job that draws it.

## 0.3.16

- Quick settings has a Power tile. It opens a Power box with Cancel, Restart and Power off, as the power button's long
  press does on the stock UI. Restart and Power off sync the storage first.

## 0.3.15

- Boot: the boot job starts the SSH server and the launcher from a copy on `/var/local` when `/mnt/us` is away, so a
  cable in at boot still shows the drive-mode picture. Eject brings the apps, settings and extensions back in the same
  launcher. The installers keep the copy up to date.

## 0.3.13

- The boot job's copy on `/var/local` (`install-mirror.sh`): the boot guard, the supervisor, the launcher, the fonts and
  the SSH server, so SSH also starts at boot without a manual step.

## 0.3.12

- Stopping the framework also stops the stock boot actions and their progress bar, so the bar no longer draws over
  the launcher.

## 0.3.11

- The boot guard stops the stock boot animation (`animator`) before the launcher takes the screen.

## 0.3.10

- The boot guard waits for the stock progress bar (the splash job) to finish again. Stopping it early delayed the
  stock UI and the launcher after it.

## 0.3.7

- The boot guard waits for the stock progress bar to finish before the launcher starts, so the bar does not draw
  over the launcher.

## 0.3.6

- USB: plugging the cable in shows the drive on the computer and the launcher's drive-mode picture in place of the
  stock USB screen; ejecting returns to the grid. The launcher and its supervisor no longer hold `/mnt/us`, which the
  Kindle unmounts for the drive, so they keep running through it.

## 0.3.5

- Nothing of the launcher holds `/mnt/us` while it runs: the supervisor runs from `/`, the launcher logs through a pipe,
  the library entry runs from a copy, and the SSH server runs from a copy in `/var/tmp`.

## 0.3.4

- The drive-mode screen shows the picture `assets/usb-drive-mode.png`.

## 0.3.3

- The launcher reads the USB state each second (cable in, charging, or the drive shown on a computer). While the drive
  is shown, its screen is drawn and taps are ignored.

## 0.3.1

- KUAL menus: an item's `if` of the form `"Model" -m` or `-m!` shows it on that model (the Oasis 3 is KindleOasis3).

## 0.3.0

- Phase 3 starts: system duties (USB, boot). The supervisor and the Kindle UI return are kept as in 0.2.x.

## 0.3.2

- Leaving the launcher always starts the Kindle UI again. A launcher that started while the framework was already
  stopped did not record that, so leaving left the screen blank.
- volumd keeps running while the launcher's grid is in front, so a USB cable gives the Kindle's drive mode, as the
  stock UI does. It is paused only while an app runs, so the app's files do not change under it.

## 0.2.24

- Settings > Device has a Launcher section with "Restart the launcher", which starts the launcher again as after an
  install.
- Wi-Fi: a network that did not connect says why: "check the password" when it was in range, "not in range now" when
  it was not. A connection that gets no address from the network is reported as such, and the join waits for one.

## 0.2.21

- The SSH server is the launcher's own: Dropbear and its zlib ship in the package (`ssh/bin`, `ssh/lib`), and the host key
  stays in `ssh/` across installs. The first install copies the key and the password flag from the browser's files, so
  nothing is lost. Primitive Browser no longer ships or runs an SSH server.
- Public DNS is the launcher's: `dns.sh`, with its backup in `dns/`; the first install keeps the browser's backup.
- Settings has a page for SSH & DNS, under Network. SSH: the switch (the quick settings' SSH switch does the same),
  the root password (set on the keyboard), the connect command while the server runs, and a check of the server.
  DNS: public DNS on or off (1.1.1.1 and 8.8.8.8), with the network's own settings kept for Restore.
- SSH opens the Kindle's firewall for port 2222 when it starts and closes it when it stops.
- Primitive Browser's Diagnostics page is gone; its SSH and DNS features are here.

## 0.2.20

- Quick settings has a System section: CPU, memory and storage, each with an icon, a bar and its value. The status
  bar no longer shows them. The icons and figures are shared with Primitive Browser (`primitive-quick`).
- Wi-Fi: "Other…" joins a network that does not show in the list (name, then password; an empty password joins an
  open network). With Wi-Fi off the list says so, and turning Wi-Fi on from the page starts a scan.
- The keyboard takes its keys, spacing and colours from the shared keyboard (`primitive-keyboard`), as in Primitive
  Browser.

## 0.2.7

- The first published release. The release workflow installs zig and cargo-zigbuild with setup actions (pinned
  to the versions the device builds use); the v0.2.6 release run failed while installing them, so 0.2.6 has a tag
  but no release package.
- The README says how to reach us if Amazon wants the Kindle icon removed.

## 0.2.6

- The library icon has the Primitive logo in its centre tile, and rounded-square tiles like the home screen.
- KOReader's library entry runs through the launcher's wrapper by default, so leaving KOReader returns to the
  launcher without starting the stock UI. `apps.toml` can still give KOReader another command.
- First version on GitHub, with CI and a release workflow.

## 0.2.0 – 0.2.5

- **Home screen in the style of a phone:** rounded-square icons of one size on a 4 × 5 grid, names under them,
  pages of 20 with dots at the bottom; swipe or the page buttons change pages.
- **Tap feedback:** only the touched icon darkens. The screen updates only the rectangle that changed.
- **Opening animation:** a card grows from the icon to the screen when an app opens; it can be turned off in
  Settings.
- **App menu:** a long press opens a box beside the icon: move earlier or later, change icon, hide.
- **Dark mode:** white on black for the home screen, Settings, menus and dialogs; picture icons keep their look.
- **Kindle UI:** the confirmation is a pop-up over the grid, and turns into "Opening the Kindle UI" with a
  progress bar until the library appears.
- **Icons:** the Kindle UI tile has the Kindle reader icon; built-in tiles have drawn icons.
- **Auto-rotation:** the screen follows the Kindle's turn (upright or upside down), read from the accelerometer
  with the stock UI stopped.
- **Sleep:** the stock screensaver shows straight away; the launcher no longer draws its own sleep screen first.
- **Status bar:** Wi-Fi and battery icons from Primitive Browser.
- **Extensions:** KUAL extensions behind one Extensions tile, with submenus; items with `exitmenu: false` run in
  the background.
- Sleep pictures from `launcher/screensavers/`; low-battery warnings at 10 % and 5 %.

## 0.1.11

- The first working launcher (Gate 1): the stock framework stops while the launcher runs; Primitive Browser and
  KOReader open and return; sleep and wake; the in-place Kindle UI return; a killed supervisor brings the stock
  UI back.
