#!/bin/sh
# Primitive Launcher boot guard (Phase 3, opt-in). Started by the upstart job /etc/upstart/primitive-launcher.conf
# after the stock framework has started. It starts the supervisor, so the launcher opens after a normal boot.
# Design: docs/02-architecture.md §2.10. Nothing here runs unless the job file is installed.
#
# Escape hatches (over USB or SSH, no program needs to run):
#   /mnt/us/launcher/DISABLE   created by this guard after 3 failed boots; delete it to re-enable
#   /mnt/us/BOOT_KINDLE        present: boot to the stock UI this time
# Once per boot: /var/tmp is tmpfs, so the marker is cleared by a reboot. Kindle mode restarts the stock
# framework, which emits "started lab126_gui" again; the marker stops the job from relaunching the launcher.
#
# Environment (tests and dry runs): KL_DIR, KL_RUN, KL_BOOT_KINDLE, KL_BOOT_ONCE, KL_HEALTHY_SECS,
# KL_SETTLE_SECS, KL_MAX_BOOT_ATTEMPTS.
DIR=${KL_DIR:-/mnt/us/launcher}
RUN=${KL_RUN:-/var/tmp/kl}
LOG=$DIR/log-launcher.txt
ATTEMPTS=$DIR/boot-attempts
DISABLE=$DIR/DISABLE
BOOT_KINDLE=${KL_BOOT_KINDLE:-/mnt/us/BOOT_KINDLE}
ONCE=${KL_BOOT_ONCE:-/var/tmp/kl-boot-once}
HEALTHY=${KL_HEALTHY_SECS:-60}
SETTLE=${KL_SETTLE_SECS:-20}
MAX_ATTEMPTS=${KL_MAX_BOOT_ATTEMPTS:-3}

log() { echo "[boot $(date '+%H:%M:%S')] $*" >> "$LOG"; }

mkdir -p "$DIR" "$RUN"

if [ -e "$ONCE" ]; then
    exit 0
fi
: > "$ONCE"

if [ -e "$DISABLE" ]; then
    log "DISABLE is present: stock UI"
    exit 0
fi
if [ -e "$BOOT_KINDLE" ]; then
    log "BOOT_KINDLE is present: stock UI"
    exit 0
fi

n=$(cat "$ATTEMPTS" 2>/dev/null)
n=$(( ${n:-0} + 1 ))
update_in_progress=no
if [ -f "$DIR/store/journal.json" ] && [ -f "$DIR/kl-launcher.prev" ]; then
    update_in_progress=yes
fi
if [ "$n" -ge "$MAX_ATTEMPTS" ] && [ "$update_in_progress" = no ]; then
    # Reset the count too, so deleting DISABLE later starts from zero.
    echo 0 > "$ATTEMPTS"
    : > "$DISABLE"
    log "boot $n: the launcher did not stay up on earlier boots; autostart disabled (DISABLE)"
    exit 0
fi
echo "$n" > "$ATTEMPTS"

supervisor=$DIR/supervisor.sh
if [ -f "$DIR/store/journal.json" ] && [ -f "$supervisor.prev" ]; then
    supervisor=$supervisor.prev
fi
if [ ! -f "$supervisor" ]; then
    log "no supervisor.sh: stock UI"
    exit 0
fi

# The stock boot progress bar (the splash job, which runs to 94 %, about 40 s) draws over whatever is on the screen,
# so the launcher waits until that job has finished. Then the stock UI draws its home screen before the supervisor
# stops it.
i=0
while status splash 2>/dev/null | grep -q "start/" && [ $i -lt 120 ]; do
    sleep 1
    i=$((i + 1))
done
sleep "$SETTLE"

# The stock boot animation (animator, drawn by BootActionsApplication) keeps drawing after the splash job has finished,
# so it would stay on top of the launcher: stop both before the launcher takes the screen.
killall -q -s KILL animator 2>/dev/null || true
killall -q -s KILL BootActionsApplication 2>/dev/null || true
cp "$supervisor" "$RUN/supervisor.sh" 2>/dev/null || exit 0
log "boot $n: starting the launcher"
sh "$RUN/supervisor.sh" >> "$LOG" 2>&1 &
sp=$!

# Stays up for HEALTHY seconds: the launcher is working, so the next boot starts again from zero.
(
    sleep "$HEALTHY"
    if kill -0 "$sp" 2>/dev/null; then
        echo 0 > "$ATTEMPTS"
        log "launcher healthy for ${HEALTHY}s: boot count reset"
    fi
) &
timer=$!

wait "$sp"
kill "$timer" 2>/dev/null
log "launcher ended (boot $n)"
exit 0
