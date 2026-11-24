#!/bin/sh
# Primitive Launcher supervisor.
#
# Started by documents/Launcher.sh (from the Kindle library) or, later, by the boot job. It runs from a
# copy in $RUN (tmpfs), so installs can replace the files on /mnt/us while it runs.
#
#   1. Stops the stock framework (lab126_gui). volumd keeps running, so a USB cable gives the Kindle's drive mode
#      as the stock UI does. While a legacy app runs, volumd is paused, so a cable
#      cannot take /mnt/us from under it. Both are recorded in marker files first.
#   2. Runs the launcher (kl-launcher, copied to $RUN). Exit 10: run the app described in $RUN/next, then
#      start the launcher again. Exit 0: the user asked for the Kindle UI. TERM (143): restart the launcher
#      (used by the installer to load a new build). Anything else is a crash: restart, up to
#      $MAX_CRASHES in $CRASH_WINDOW seconds, then give up.
#   3. On leaving, for any reason, resumes volumd and starts the framework again (also when it was already
#      stopped at start: the framework must not be left down).
#
# A watchdog process outlives this script. If the supervisor dies (killed, crashed), the watchdog gives
# the UI back. It also kills a launcher whose heartbeat file stops changing.
#
# Environment: KL_DIR (default /mnt/us/launcher), KL_RUN (default /var/tmp/kl), KL_DOCUMENTS.

DIR=${KL_DIR:-/mnt/us/launcher}
RUN=${KL_RUN:-/var/tmp/kl}
DOCUMENTS=${KL_DOCUMENTS:-/mnt/us/documents}
LOG=$DIR/log-launcher.txt
MARK=$RUN/framework-stopped
VOLUMD_MARK=$RUN/volumd-paused
PIDFILE=$RUN/supervisor.pid
LAUNCHER_PID=$RUN/launcher.pid
LAUNCHER_STATUS=$RUN/launcher.status
APP_PID=$RUN/app.pid
APP_TURN_OWNER=$RUN/app-turn-owner
app_turn_dir=
HOLDER_PID=$RUN/holder.pid
WAIT_PID=$RUN/wait.pid
APP_WATCH=$RUN/app-watch.pid
APP_REQUEST=$RUN/app-request
APP_RESTART=$RUN/app-restart
# Starts an app in its own process group (so a signal reaches the app itself), when the system has setsid.
APP_GROUP=$(command -v setsid 2>/dev/null || true)
MAX_CRASHES=${KL_MAX_CRASHES:-3}
CRASH_WINDOW=60
HEARTBEAT_LIMIT=${KL_HEARTBEAT_LIMIT:-30}
WATCH_EVERY=${KL_WATCH_EVERY:-5}
UPDATE_HEALTHY=${KL_UPDATE_HEALTHY_SECS:-10}
UPDATE_JOURNAL=$DIR/store/journal.json
UPDATE_DRIVER=$RUN/launcher-update-driver
UPDATE_SETUP=$RUN/launcher-update-setup
UPDATE_READY=$RUN/launcher-update-ready
STORE_OPEN=$RUN/store-open
STORE_CHILD=$RUN/store-child.pid
STORE_STORAGE_REQUEST=$RUN/store-storage-request
STORE_STORAGE_REPLY=$RUN/store-storage-reply
STORE_USB_PROC=${KL_STORE_USB_PROC:-/proc}
LAUNCHER_USB=$RUN/launcher-usb
NATIVE_APP_RUN=$RUN/native-app

mkdir -p "$RUN"
export KL_DIR="$DIR" KL_RUN="$RUN" KL_DOCUMENTS="$DOCUMENTS" KL_LAUNCHER_SELF_UPDATE=1
# Nothing of this script may hold /mnt/us: the Kindle unmounts it to show its drive on a computer (a USB cable), and
# it kills whatever still holds it. The working directory is /, and long-running programs log through a pipe.
cd /
LOG_FIFO=$RUN/log.fifo
LOG_PENDING=$RUN/log-pending.txt
LOG_OUT=$LOG

log() { echo "[launcher $(date '+%H:%M:%S')] $*" >> "$LOG"; }

# The launcher, the holder and the wait screen write into a pipe; this reader appends each line to the log and closes
# the log again, so no long-running program keeps a file open on /mnt/us. While /mnt/us is away (the drive is on a
# computer), lines wait in $LOG_PENDING and are added when it is back.
logger_start() {
    rm -f "$LOG_FIFO"
    mkfifo "$LOG_FIFO" 2>/dev/null || return 0
    sup=$$
    (
        while [ -p "$LOG_FIFO" ] && kill -0 "$sup" 2>/dev/null; do
            while IFS= read -r line; do
                if [ -s "$LOG_PENDING" ] && cat "$LOG_PENDING" >> "$LOG" 2>/dev/null; then
                    rm -f "$LOG_PENDING"
                fi
                echo "$line" >> "$LOG" 2>/dev/null || echo "$line" >> "$LOG_PENDING"
            done < "$LOG_FIFO"
        done
    ) &
    LOGGER=$!
    LOG_OUT=$LOG_FIFO
}

logger_stop() {
    [ -n "${LOGGER:-}" ] && kill "$LOGGER" 2>/dev/null
    rm -f "$LOG_FIFO"
    LOG_OUT=$LOG
}

# Keep the log small on the Kindle's flash: keep the last ~100 KB of a log over ~200 KB.
if [ -f "$LOG" ] && [ "$(wc -c < "$LOG")" -gt 204800 ]; then
    tail -c 102400 "$LOG" > "$LOG.tmp" && mv "$LOG.tmp" "$LOG"
fi

if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE" 2>/dev/null)" 2>/dev/null; then
    log "already running (pid $(cat "$PIDFILE")); nothing to do"
    exit 0
fi
echo $$ > "$PIDFILE"
rm -f "$STORE_OPEN" "$STORE_OPEN.tmp"
log "start (pid $$)"

if [ -e /etc/init.d/framework ]; then INIT=sysv; else INIT=upstart; fi

pause_us() { usleep "$1" 2>/dev/null || sleep 1; }

framework_running() {
    if [ "$INIT" = upstart ] && command -v status >/dev/null 2>&1; then
        status lab126_gui 2>/dev/null | grep -q "start/" && return 0
    fi
    pidof cvm >/dev/null 2>&1
}

install_traps() {
    trap 'on_signal' TERM INT HUP
}

# Holds com.lab126.kaf frameworkStarted while the framework is stopped, so powerd answers the power button
# (see holder.lua). Optional: without lua or holder.lua the launcher still runs, but the button may not sleep.
holder_start() {
    holder_stop
    if ! refresh_copy holder.lua "${1:-}" && [ "${1:-}" = verify ]; then
        log "holder not started: could not refresh holder.lua"
        return
    fi
    if [ ! -f "$RUN/holder.lua" ] || ! command -v lua >/dev/null 2>&1; then
        log "holder not started: lua or holder.lua missing"
        return
    fi
    lua "$RUN/holder.lua" >> "$LOG_OUT" 2>&1 &
    echo $! > "$HOLDER_PID"
    log "holder started (pid $!)"
}

# The holder must be gone before the framework starts: it holds com.lab126.kaf, the framework's own lipc name,
# and the framework cannot register while it is taken ("Duplicate service name"). TERM first, then KILL.
holder_stop() {
    hp=$(cat "$HOLDER_PID" 2>/dev/null)
    if [ -n "$hp" ]; then
        kill "$hp" 2>/dev/null
        i=0
        while kill -0 "$hp" 2>/dev/null && [ $i -lt 10 ]; do
            pause_us 200000
            i=$((i + 1))
        done
        if kill -0 "$hp" 2>/dev/null; then
            kill -KILL "$hp" 2>/dev/null
            log "holder did not stop on TERM; killed (pid $hp)"
        else
            log "holder stopped (pid $hp)"
        fi
    fi
    rm -f "$HOLDER_PID"
}

framework_stop() {
    log "stopping the framework ($INIT)"
    echo stopped > "$MARK"
    t0=$(date +%s)
    # Stopping the framework can signal processes it started; this script must survive that.
    trap "" TERM
    if [ "$INIT" = sysv ]; then
        /etc/init.d/framework stop >> "$LOG" 2>&1
    else
        stop lab126_gui >> "$LOG" 2>&1
        pause_us 1250000
        # Stopping the framework starts the stock boot actions again, and their progress bar (BootActionsApplication,
        # drawn by animator) would draw over the launcher: stop them as well.
        stop bootactions >> "$LOG" 2>&1 || true
        killall -q -s KILL animator 2>/dev/null || true
        killall -q -s KILL BootActionsApplication 2>/dev/null || true
    fi
    install_traps
    log "framework stopped after $(( $(date +%s) - t0 ))s"
    holder_start "${1:-}"
}

# The stock lipc-daemon answers com.lab126.system properties (usid and others) by forking small scripts. It
# has been seen wedged after a launcher session: every script hangs, the framework never becomes ready (it
# waits for usid), and the library shows "application error". Check it before starting the framework, and
# restart it if it does not answer. The state is logged to help find what wedges it.
usid_answers() {
    if command -v timeout >/dev/null 2>&1; then
        timeout 5 lipc-get-prop com.lab126.system usid >/dev/null 2>&1
    else
        lipc-get-prop com.lab126.system usid >/dev/null 2>&1
    fi
}

lipcd_check() {
    [ "$INIT" = upstart ] || return 0
    command -v lipc-get-prop >/dev/null 2>&1 || return 0
    usid_answers && return 0
    log "lipc-daemon does not answer (usid); restarting lipcd"
    for p in $(pidof lipc-daemon 2>/dev/null); do
        log "lipc-daemon $p state $(awk '{print $3}' /proc/$p/stat 2>/dev/null) wchan $(cat /proc/$p/wchan 2>/dev/null)"
    done
    log "volumd state $(awk '{print $3}' /proc/$(pidof volumd 2>/dev/null)/stat 2>/dev/null)"
    restart lipcd >> "$LOG" 2>&1 || { stop lipcd; start lipcd; } >> "$LOG" 2>&1
    pause_us 2000000
    if usid_answers; then
        log "lipc-daemon answers again"
    else
        log "lipc-daemon still does not answer"
    fi
}

framework_start() {
    log "starting the framework ($INIT)"
    lipcd_check
    t0=$(date +%s)
    if [ "$INIT" = sysv ]; then
        (cd / && /etc/init.d/framework start) >> "$LOG" 2>&1
    else
        (cd / && start lab126_gui) >> "$LOG" 2>&1
    fi
    rm -f "$MARK"
    log "framework start returned after $(( $(date +%s) - t0 ))s"
}

volumd_pause() {
    if pidof volumd >/dev/null 2>&1; then
        killall -STOP volumd 2>/dev/null && echo paused > "$VOLUMD_MARK"
    fi
}

volumd_resume() {
    # Resumed whether or not the marker is there: a volumd stopped by an earlier run must run again.
    killall -CONT volumd 2>/dev/null
    rm -f "$VOLUMD_MARK"
}

store_storage_clear() {
    rm -f "$STORE_CHILD" "$STORE_STORAGE_REQUEST" "$STORE_STORAGE_REQUEST.tmp" \
        "$STORE_STORAGE_REPLY" "$STORE_STORAGE_REPLY.tmp"
    rm -rf "$NATIVE_APP_RUN"
}

store_storage_mounted() {
    # /mnt/us itself is FUSE and stays mounted when the backing partition is exported over USB.
    awk '$2 == "/mnt/base-us" {mounted=1} END {exit !mounted}' "$STORE_USB_PROC/mounts" 2>/dev/null
}

store_storage_pause() {
    store_storage_mounted && volumd_pause || return 1
    store_poll=0
    while [ "$store_poll" -lt 3 ]; do
        store_volumd=$(pidof volumd 2>/dev/null) || return 1
        [ -n "$store_volumd" ] || return 1
        store_stopped=yes
        for store_vpid in $store_volumd; do
            [ "$(awk '$1 == "State:" {print $2}' "$STORE_USB_PROC/$store_vpid/status" 2>/dev/null)" = T ] || store_stopped=no
        done
        if [ "$store_stopped" = yes ]; then
            store_storage_mounted
            return
        fi
        store_poll=$((store_poll + 1))
        [ "$store_poll" -eq 3 ] || pause_us 100000
    done
    return 1
}

store_child_alive() {
    kill -0 "$1" 2>/dev/null || return 1
    # A reparented child can remain a zombie after it has released all of its files.
    [ "$(awk '$1 == "State:" {print $2}' "/proc/$1/status" 2>/dev/null)" != Z ]
}

store_child_started_at() {
    # Remove the parenthesized process name before reading the kernel's start time (field 22).
    awk '{sub(/^.*\) /, ""); print $20}' "/proc/$1/stat" 2>/dev/null
}

store_child_stop() {
    if [ ! -f "$STORE_CHILD" ]; then
        store_storage_clear
        return
    fi
    read -r store_pid store_started_at < "$STORE_CHILD"
    case "$store_pid" in ''|0|1|*[!0-9]*) return ;; esac
    if [ -n "$store_started_at" ] && [ "$(store_child_started_at "$store_pid")" != "$store_started_at" ]; then
        store_storage_clear
        return
    fi
    if store_child_alive "$store_pid"; then
        kill -TERM "$store_pid" 2>/dev/null
        store_poll=0
        while store_child_alive "$store_pid" && [ "$store_poll" -lt 5 ]; do
            pause_us 100000
            store_poll=$((store_poll + 1))
        done
        if store_child_alive "$store_pid"; then
            kill -KILL "$store_pid" 2>/dev/null
            # Keep USB paused until the app has stopped touching storage, even during shutdown.
            while store_child_alive "$store_pid"; do sleep 1; done
        fi
    fi
    store_storage_clear
}

store_storage_release() {
    # A prepared Launcher update takes over the paused volume at the next Launcher start.
    [ -f "$UPDATE_JOURNAL" ] || volumd_resume
}

storage_request() {
    storage_request_path=$2/store-storage-request
    storage_reply_path=$2/store-storage-reply
    [ -f "$storage_request_path" ] || return 0
    read -r store_request_pid store_seq store_state store_extra < "$storage_request_path"
    case "$store_seq" in ''|*[!0-9]*|??????????*) return 0 ;; esac
    [ "$store_request_pid" = "$1" ] && [ -z "$store_extra" ] && [ "$store_seq" -gt "$store_seq_seen" ] || return 0
    if [ "$3" = launcher ]; then
        mkdir "$LAUNCHER_USB/handling" 2>/dev/null || return 0
        if [ "$(cat "$LAUNCHER_PID" 2>/dev/null)" != "$1" ]; then
            rmdir "$LAUNCHER_USB/handling"
            return 0
        fi
    fi
    store_seq_seen=$store_seq
    case "$store_state" in
        idle)
            if [ "$3" != launcher ] || [ -f "$LAUNCHER_USB/held" ]; then
                store_storage_release
                [ "$3" != launcher ] || rm -f "$LAUNCHER_USB/held"
            fi
            ;;
        write)
            [ "$3" != launcher ] || : > "$LAUNCHER_USB/held"
            if store_storage_pause; then
                store_reply=paused
            else
                store_storage_release
                [ "$3" != launcher ] || rm -f "$LAUNCHER_USB/held"
                store_reply=unavailable
            fi
            printf '%s %s %s\n' "$1" "$store_seq" "$store_reply" > "$storage_reply_path.tmp" &&
                mv "$storage_reply_path.tmp" "$storage_reply_path"
            ;;
    esac
    [ "$3" != launcher ] || rmdir "$LAUNCHER_USB/handling"
}

store_storage_watch() {
    store_seq_seen=0
    while store_child_alive "$1"; do
        system_layer_check
        storage_request "$1" "$RUN" "$2"
        if [ "$2" = native ]; then
            pause_us 100000
        else
            sleep 1
        fi
    done
}

# The Kindle UI is ready only after the framework emits framework_ready (about 40 s after the start), the
# library app (job kppmainapp) starts, registers its lipc service com.lab126.KPPMainApp, and then its
# properties (load, go). If this script ends earlier, the library's script runner ends with it and appmgrd sends
# "load" to a library that cannot take it yet ("ServiceUnknown", then "no such property"): "application error".
# Wait until the library app offers "load", up to 120 s, then a little longer.
library_registered() {
    command -v lipc-probe >/dev/null 2>&1 || return 0
    if command -v timeout >/dev/null 2>&1; then
        timeout 5 lipc-probe com.lab126.KPPMainApp 2>/dev/null | grep -qw "load"
    else
        lipc-probe com.lab126.KPPMainApp 2>/dev/null | grep -qw "load"
    fi
}

framework_wait_ready() {
    [ "$INIT" = upstart ] || return 0
    command -v initctl >/dev/null 2>&1 || return 0
    i=0
    while [ $i -lt 120 ]; do
        if initctl status kppmainapp 2>/dev/null | grep -q "start/running" && library_registered; then
            sleep 2
            log "the Kindle UI is ready after ${i}s"
            return 0
        fi
        sleep 1
        i=$((i + 1))
    done
    log "the Kindle UI was not ready after 120s"
}

# The waiting screen ("Opening the Kindle UI" with a moving bar) covers the minute the stock UI takes to start.
wait_screen_start() {
    [ "${launcher_cache_failed:-no}" = no ] && [ -x "$RUN/kl-launcher" ] || return 0
    "$RUN/kl-launcher" --wait-screen >> "$LOG_OUT" 2>&1 &
    echo $! > "$WAIT_PID"
}

wait_screen_stop() {
    wp=$(cat "$WAIT_PID" 2>/dev/null)
    if [ -n "$wp" ]; then
        kill "$wp" 2>/dev/null
        pause_us 300000
        kill -0 "$wp" 2>/dev/null && kill -KILL "$wp" 2>/dev/null
        log "wait screen stopped"
    fi
    rm -f "$WAIT_PID"
}

# Give the UI back: undo exactly what the markers record.
restore() {
    store_child_stop
    volumd_resume
    holder_stop
    if [ -f "$MARK" ] || ! framework_running; then
        wait_screen_start
        framework_start
        framework_wait_ready
        wait_screen_stop
    fi
}

# Check bytes on a cold start: archived files can have timestamps older than the RAM copy.
# Later starts use the timestamp shortcut, while update transactions explicitly force the copy.
refresh_copy() {
    if [ "${2:-}" = force ] || [ ! -f "$RUN/$1" ] || [ "$DIR/$1" -nt "$RUN/$1" ] ||
        { [ "${2:-}" = verify ] && [ -f "$DIR/$1" ] && ! cmp -s "$DIR/$1" "$RUN/$1" 2>/dev/null; }; then
        cp "$DIR/$1" "$RUN/$1.new" 2>/dev/null && mv "$RUN/$1.new" "$RUN/$1" || return 1
        log "copied $1 to $RUN" || true
    fi
}

refresh_files() {
    mkdir -p "$RUN/fonts"
    if ! refresh_copy kl-launcher "$1" && [ "$1" = verify ]; then
        return 1
    fi
    chmod 755 "$RUN/kl-launcher" 2>/dev/null
    refresh_copy fonts/NotoSans-Regular.ttf "$1"
    refresh_copy fonts/NotoSans-Bold.ttf "$1"
    refresh_copy holder.lua
    return 0
}

# The driver stays the pre-update launcher throughout the transaction. A cold restart can recover it from
# the engine's durable backup. These helpers never execute a shell command from the journal.
update_driver() {
    if [ ! -x "$UPDATE_DRIVER" ]; then
        binary=$DIR/kl-launcher
        [ ! -x "$binary.prev" ] || binary=$binary.prev
        cp "$binary" "$UPDATE_DRIVER.new" && chmod 755 "$UPDATE_DRIVER.new" &&
            mv "$UPDATE_DRIVER.new" "$UPDATE_DRIVER" || return 1
    fi
    if [ ! -f "$UPDATE_SETUP" ]; then
        setup=$DIR/setup-boot.sh
        [ ! -f "$setup.prev" ] || setup=$setup.prev
        cp "$setup" "$UPDATE_SETUP.new" && mv "$UPDATE_SETUP.new" "$UPDATE_SETUP" || return 1
    fi
}

update_copy() {
    mkdir -p "$RUN/fonts" &&
        refresh_copy kl-launcher force && chmod 755 "$RUN/kl-launcher" &&
        refresh_copy fonts/NotoSans-Regular.ttf force &&
        refresh_copy fonts/NotoSans-Bold.ttf force || return 1
    [ ! -f "$DIR/holder.lua" ] || refresh_copy holder.lua force
}

update_clear() {
    rm -f "$UPDATE_DRIVER" "$UPDATE_SETUP" "$UPDATE_READY"
    trial=0
    volumd_resume
}

update_revert() {
    "$UPDATE_DRIVER" --launcher-update revert >> "$LOG_OUT" 2>&1 || return 1
    update_copy || return 1
    if [ "$mirror_changed" = yes ]; then
        KL_SETUP_BOOT_STRICT=1 sh "$UPDATE_SETUP" "$DIR" >> "$LOG_OUT" 2>&1 ||
            log "launcher update: old boot copy still needs refreshing"
    fi
    log "launcher update: previous files restored"
    update_clear
}

update_begin() {
    trial=0
    mirror_changed=no
    [ -f "$UPDATE_JOURNAL" ] || return 0
    volumd_pause
    if ! update_driver; then
        log "launcher update: could not copy the recovery tools"
        return 1
    fi
    update_state=$("$UPDATE_DRIVER" --launcher-update begin 2>> "$LOG") || {
        log "launcher update: could not start the trial"
        return 1
    }
    case "$update_state" in
        idle) update_clear ;;
        reverted)
            update_copy || return 1
            KL_SETUP_BOOT_STRICT=1 sh "$UPDATE_SETUP" "$DIR" >> "$LOG_OUT" 2>&1 ||
                log "launcher update: old boot copy still needs refreshing"
            log "launcher update: previous files restored after failed starts"
            update_clear
            ;;
        trial)
            if update_copy; then
                trial=1
            else
                log "launcher update: could not copy the candidate to temporary storage"
                update_revert || return 1
            fi
            ;;
        *) log "launcher update: unrecognized trial state"; return 1 ;;
    esac
}

# The ready marker holds the launcher's own pid (not its wrapper's): from $LAUNCHER_PID while it runs, and from
# $launcher_proc once it has ended.
update_ready() {
    p=${launcher_proc:-$(cat "$LAUNCHER_PID" 2>/dev/null)}
    [ -n "$p" ] && [ "$(cat "$UPDATE_READY" 2>/dev/null)" = "$p" ]
}

update_healthy() {
    update_ready || return 1
    hb=$(cat "$RUN/heartbeat" 2>/dev/null)
    case "$hb" in ''|*[!0-9]*) return 1 ;; esac
    now=$(date +%s)
    [ "$hb" -gt "$launch_started" ] &&
        [ "$hb" -le "$now" ] &&
        [ $((now - hb)) -le "$HEARTBEAT_LIMIT" ] &&
        [ $((now - launch_started)) -ge "$UPDATE_HEALTHY" ]
}

update_accept() {
    mirror_changed=yes
    KL_SETUP_BOOT_STRICT=1 sh "$UPDATE_SETUP" "$DIR" >> "$LOG_OUT" 2>&1 || {
        log "launcher update: could not refresh the boot copy"
        return 1
    }
    "$UPDATE_DRIVER" --launcher-update accept >> "$LOG_OUT" 2>&1 || {
        log "launcher update: could not finish the transaction"
        return 1
    }
    log "launcher update: healthy, boot copy refreshed"
    update_clear
}

# Logs what the USB cable and the drive do, and whether the launcher and this script are alive, each time one of
# them changes. It is the record for a launcher that disappears during a USB session.
usb_watch() {
    last=""
    previous_lp=""
    store_seq_seen=0
    while kill -0 "$1" 2>/dev/null; do
        cable=$(cat /sys/class/power_supply/max77796-charger/online 2>/dev/null)
        mounted=$(grep -c " /mnt/base-us " /proc/mounts 2>/dev/null)
        vstate=$(awk '{print $3}' /proc/$(pidof volumd 2>/dev/null)/stat 2>/dev/null)
        lp=$(cat "$LAUNCHER_PID" 2>/dev/null)
        lalive=no
        [ -n "$lp" ] && kill -0 "$lp" 2>/dev/null && lalive=yes
        if [ "$lp" != "$previous_lp" ]; then
            previous_lp=$lp
            store_seq_seen=0
        fi
        [ "$lalive" != yes ] || storage_request "$lp" "$LAUNCHER_USB" launcher
        now="cable=$cable userstore=${mounted:-?} volumd=${vstate:-none} launcher=${lp:-none}/$lalive supervisor=$1"
        if [ "$now" != "$last" ]; then
            log "usb: $now"
            last="$now"
        fi
        sleep 1
    done
}

launcher_storage_end() {
    # The Launcher PID was removed. Let a request already being serviced finish before a
    # separately supervised app can take over user storage.
    launcher_usb_wait=0
    while [ -d "$LAUNCHER_USB/handling" ] && [ "$launcher_usb_wait" -lt 10 ]; do
        pause_us 100000
        launcher_usb_wait=$((launcher_usb_wait + 1))
    done
    if [ -d "$LAUNCHER_USB/handling" ]; then
        kill "$USB_MONITOR" 2>/dev/null
        wait "$USB_MONITOR" 2>/dev/null
        rmdir "$LAUNCHER_USB/handling"
        usb_watch "$$" &
        USB_MONITOR=$!
    fi
    if [ -f "$LAUNCHER_USB/held" ]; then
        store_storage_release
        rm -f "$LAUNCHER_USB/held"
    fi
    rm -f "$LAUNCHER_USB/store-storage-request" "$LAUNCHER_USB/store-storage-request.tmp" \
        "$LAUNCHER_USB/store-storage-reply" "$LAUNCHER_USB/store-storage-reply.tmp"
}

watchdog() {
    trap '' HUP INT
    sup=$1
    strikes=0
    while kill -0 "$sup" 2>/dev/null; do
        sleep "$WATCH_EVERY"
        lp=$(cat "$LAUNCHER_PID" 2>/dev/null)
        if [ -n "$lp" ] && kill -0 "$lp" 2>/dev/null; then
            hb=$(cat "$RUN/heartbeat" 2>/dev/null)
            age=$(( $(date +%s) - ${hb:-0} ))
            # Two checks in a row: after a wake from sleep the heartbeat is old until the launcher's next tick.
            if [ "$age" -gt "$HEARTBEAT_LIMIT" ]; then
                strikes=$((strikes + 1))
                if [ "$strikes" -ge 2 ]; then
                    echo "[watchdog $(date '+%H:%M:%S')] no heartbeat for ${age}s; stopping the launcher (pid $lp)" >> "$LOG"
                    kill -KILL "$lp" 2>/dev/null
                    strikes=0
                fi
            else
                strikes=0
            fi
        else
            strikes=0
        fi
    done
    if [ -f "$MARK" ] || [ -f "$VOLUMD_MARK" ]; then
        echo "[watchdog $(date '+%H:%M:%S')] supervisor gone; giving the UI back" >> "$LOG"
        restore
    fi
}

finish() {
    [ -z "${USB_MONITOR:-}" ] || kill "$USB_MONITOR" 2>/dev/null
    restore
    wait_screen_stop
    logger_stop
    rm -f "$PIDFILE" "$LAUNCHER_PID" "$APP_PID" "$HOLDER_PID" "$STORE_OPEN" "$STORE_OPEN.tmp"
    clear_app_turn
    log "end"
}

# The path given to an app is unique to this run. A late write from an old child
# can never supply the turn of a later app, even if the kernel reuses its PID.
clear_app_turn() {
    rm -f "$APP_TURN_OWNER" "$APP_TURN_OWNER.tmp"
    if [ -n "${app_turn_dir:-}" ]; then
        rm -f "$app_turn_dir/turn" "$app_turn_dir/turn.tmp"
        rmdir "$app_turn_dir" 2>/dev/null || :
        app_turn_dir=
    fi
}

on_signal() {
    log "signal received; stopping"
    for f in "$LAUNCHER_PID" "$APP_PID"; do
        p=$(cat "$f" 2>/dev/null)
        [ -n "$p" ] && kill -TERM "$p" 2>/dev/null
    done
    finish
    exit 0
}

# An app that redraws itself on wake declares it: a line "# Wake: redraw" in the first ten lines of its entry. The
# supervisor then sends SIGUSR1 to the app's process group. Other apps are restarted on wake, so the screen is never
# left blank.
# Stop a process and its children (busybox has no pkill -P).
kill_tree() {
    for child in $(ps -o pid,ppid 2>/dev/null | awk -v p="$1" '$2 == p { print $1 }'); do
        kill_tree "$child"
    done
    kill "$1" 2>/dev/null
}

app_redraws_on_wake() {
    [ -f "$1" ] && [ ! -L "$1" ] && head -n 10 "$1" | grep -Fxq '# Wake: redraw'
}

# While an app runs the launcher is not, so the supervisor keeps the sleep and wake behaviour: it draws the sleep
# picture when the Kindle goes to sleep, and redraws or restarts the app when it wakes.
app_power_watch() {
    pgid=$1
    redraw=$2
    command -v lipc-wait-event >/dev/null 2>&1 || return 0
    lipc-wait-event -m com.lab126.powerd goingToScreenSaver,outOfScreenSaver 2>/dev/null |
    while IFS= read -r event; do
        # A watcher acts only for its own app: one left behind by an earlier app must never restart the next.
        [ "$(cat "$APP_PID" 2>/dev/null)" = "$pgid" ] || break
        case "$event" in
            goingToScreenSaver*)
                [ -x "$RUN/kl-launcher" ] && "$RUN/kl-launcher" --sleep-screen >> "$LOG_OUT" 2>&1
                ;;
            outOfScreenSaver*)
                if [ "$redraw" = yes ]; then
                    kill -USR1 -- "-$pgid" 2>/dev/null || kill -USR1 "$pgid" 2>/dev/null
                else
                    : > "$APP_RESTART"
                    kill -TERM -- "-$pgid" 2>/dev/null || kill -TERM "$pgid" 2>/dev/null
                fi
                ;;
        esac
    done
}

# Start the launcher (the system layer). $1 is 1 when an app already runs: the launcher follows it instead of drawing
# the home screen. A wrapper waits for the launcher and sends USR2 when it ends, so the supervisor sees the end while
# it waits for an app; $lp is the wrapper, and $LAUNCHER_PID holds the launcher itself.
start_launcher() {
    launcher_proc=
    rm -f "$LAUNCHER_PID" "$LAUNCHER_STATUS" "$LAUNCHER_STATUS.tmp"
    (
        KL_SYSTEM_LAYER=1 KL_ATTACH_APP=$1 KL_SHOW_STARTUP_WALLPAPER=$startup_wallpaper KL_LAUNCHER_UPDATE_TRIAL=$trial KL_LAUNCHER_USB_LEASE=$launcher_can_lease "$RUN/kl-launcher" >> "$LOG_OUT" 2>&1 &
        launcher=$!
        echo "$launcher" > "$LAUNCHER_PID"
        # A signal can make wait return early, so wait again until the launcher has really ended.
        wait "$launcher"
        status=$?
        while kill -0 "$launcher" 2>/dev/null; do
            wait "$launcher"
            status=$?
        done
        echo "$status" > "$LAUNCHER_STATUS.tmp" && mv -f "$LAUNCHER_STATUS.tmp" "$LAUNCHER_STATUS"
        kill -USR2 "$SUPERVISOR" 2>/dev/null
        exit "$status"
    ) &
    lp=$!
}

# Signal the launcher itself (not its wrapper).
launcher_signal() {
    p=$(cat "$LAUNCHER_PID" 2>/dev/null)
    [ -n "$p" ] && kill "-$1" "$p" 2>/dev/null
}

# The launcher ended while the app ($1, a process group; $2 yes when it redraws) runs. The Kindle UI closes the app
# first; any other end starts the launcher again over the app, so the power button, sleep and quick settings keep
# working. A launcher that keeps crashing leaves sleep and wake to the supervisor until the app ends.
system_layer_ended() {
    while kill -0 "$lp" 2>/dev/null; do
        wait "$lp" 2>/dev/null
    done
    lrc=$(cat "$LAUNCHER_STATUS" 2>/dev/null || echo 1)
    rm -f "$LAUNCHER_PID" "$LAUNCHER_STATUS"
    # The launcher may have ended with the app frozen under quick settings.
    kill -CONT -- "-$1" 2>/dev/null || kill -CONT "$1" 2>/dev/null
    if [ "$lrc" -eq 0 ]; then
        log "the Kindle UI was requested over the app; closing the app"
        launcher_rc=0
        kill -TERM -- "-$1" 2>/dev/null || kill -TERM "$1" 2>/dev/null
        return
    fi
    now=$(date +%s)
    recent=""
    for t in $crashes; do
        [ $((now - t)) -lt "$CRASH_WINDOW" ] && recent="$recent $t"
    done
    crashes="$recent $now"
    count=$(echo $crashes | wc -w)
    log "the launcher ended over the app (rc=$lrc), $count in ${CRASH_WINDOW}s"
    if [ "$count" -ge "$MAX_CRASHES" ]; then
        log "too many launcher crashes; the supervisor keeps sleep and wake until the app ends"
        launcher_rc=$lrc
        resident_app=no
        app_power_watch "$1" "$2" &
        echo $! > "$APP_WATCH"
        return
    fi
    # Whatever the launcher had drawn over the app goes: the app draws its whole screen again.
    [ "$2" = yes ] && { kill -USR1 -- "-$1" 2>/dev/null || kill -USR1 "$1" 2>/dev/null; }
    start_launcher 1
    log "the launcher started again over the app"
}

# Called from every loop that runs while an app does (the wait for the app, and the storage watch of an app with a
# storage lease): the launcher's end is handled at once, whichever loop the supervisor is in.
system_layer_check() {
    if [ "$launcher_ended" = yes ] && [ "$resident_app" = yes ]; then
        launcher_ended=no
        system_layer_ended "$apid" "$app_redraw"
    fi
}

run_app() {
    if [ ! -f "$RUN/next" ]; then
        log "launch requested but $RUN/next is missing"
        store_storage_release
        return
    fi
    # A fourth line may open a specific screen in the standalone Store.
    { read -r adir; read -r kind; read -r arg; IFS= read -r start_tab || :; } < "$RUN/next"
    # A restart flag belongs to one app run: one left from an earlier run must not reopen this app.
    rm -f "$APP_RESTART"
    # Kept so a restart after wake can open the same app again.
    printf '%s\n%s\n%s\n%s\n' "$adir" "$kind" "$arg" "$start_tab" > "$APP_REQUEST.tmp" && mv "$APP_REQUEST.tmp" "$APP_REQUEST"
    rm -f "$RUN/next" "$STORE_OPEN" "$STORE_OPEN.tmp"
    store_storage_clear
    store_can_open=0
    store_can_lease=0
    store_start_tab=
    native_can_lease=0
    app_log=$LOG
    # Keep volumd paused through launch, and until a storage-aware app reports it is idle.
    volumd_pause
    if [ "$kind" = script ] && [ "$arg" = "$DOCUMENTS/Store.sh" ]; then
        store_can_open=1
        if [ "$start_tab" = installed ] && [ -f "$arg" ] && [ ! -L "$arg" ]; then
            store_start_tab=installed
        fi
        # Only advertise idle USB when the Store cannot retain a descriptor on the flash log.
        if [ -p "$LOG_OUT" ]; then
            store_can_lease=1
            app_log=$LOG_OUT
        fi
    elif [ "$kind" = script ] && [ -p "$LOG_OUT" ]; then
        case "$arg" in
            "$DOCUMENTS/"*.sh)
                if [ -f "$arg" ] && [ ! -L "$arg" ] &&
                    head -n 10 "$arg" | grep -Fxq '# PrimitiveUSBLease' &&
                    mkdir "$NATIVE_APP_RUN" && cp "$arg" "$NATIVE_APP_RUN/launch"; then
                    native_can_lease=1
                    app_log=$LOG_OUT
                fi
                ;;
        esac
    fi
    log "app start: $kind $arg (in $adir)"
    t0=$(date +%s)
    clear_app_turn
    app_turn_dir=$(mktemp -d "$RUN/app-turn.XXXXXX" 2>/dev/null) || app_turn_dir=
    (
        cd "$adir" 2>/dev/null || cd /mnt/us 2>/dev/null || cd /
        export PRIMITIVE_LAUNCHER=1 PRIMITIVE_FRAMEWORK=stopped KL_STORE_OPEN_APP="$store_can_open"
        export KL_STORE_USB_LEASE="$store_can_lease"
        export KL_STORE_START_TAB="$store_start_tab"
        unset KL_APP_TURN_FILE
        if [ -n "$app_turn_dir" ]; then
            export KL_APP_TURN_FILE="$app_turn_dir/turn"
        fi
        unset KL_NATIVE_APP_RUN
        if [ "$native_can_lease" -eq 1 ]; then
            export KL_NATIVE_APP_RUN="$NATIVE_APP_RUN"
            cd / || exit 1
            exec $APP_GROUP /bin/sh "$NATIVE_APP_RUN/launch"
        fi
        case "$kind" in
            script) exec $APP_GROUP /bin/sh "$arg" ;;
            hooks) exec $APP_GROUP /bin/sh -c '. "$1"; on_run' sh "$arg" ;;
            shell) exec $APP_GROUP /bin/sh -c "$arg" ;;
            *) echo "unknown kind: $kind"; exit 1 ;;
        esac
    ) >> "$app_log" 2>&1 &
    apid=$!
    echo "$apid" > "$APP_PID"
    if [ -n "$app_turn_dir" ]; then
        app_start=$(store_child_started_at "$apid")
        if [ -n "$app_start" ]; then
            printf 'v1 %s %s %s\n' "$apid" "$app_start" "${app_turn_dir##*/}" > "$APP_TURN_OWNER.tmp" &&
                mv "$APP_TURN_OWNER.tmp" "$APP_TURN_OWNER"
        fi
    fi
    app_redraw=no
    [ "$kind" = script ] && app_redraws_on_wake "$arg" && app_redraw=yes
    # The launcher reads this before it asks the app to redraw: SIGUSR1 ends an app that does not handle it.
    if [ "$app_redraw" = yes ]; then : > "$RUN/app-redraws"; else rm -f "$RUN/app-redraws"; fi
    # A launcher that stays (the system layer) keeps sleep and wake itself; otherwise the supervisor does.
    if [ "$resident_app" != yes ]; then
        app_power_watch "$apid" "$app_redraw" &
        echo $! > "$APP_WATCH"
    fi
    if [ "$store_can_open" -eq 1 ] || [ "$native_can_lease" -eq 1 ]; then
        printf '%s %s\n' "$apid" "$(store_child_started_at "$apid")" > "$STORE_CHILD"
        if [ "$native_can_lease" -eq 1 ]; then
            store_storage_watch "$apid" native
        elif [ "$store_can_lease" -eq 1 ]; then
            store_storage_watch "$apid" store
        fi
    fi
    # The launcher's launch request (SIGUSR1) interrupts wait while the app still runs: wait again until it ends.
    while :; do
        wait "$apid"
        rc=$?
        system_layer_check
        kill -0 "$apid" 2>/dev/null || break
    done
    rm -f "$APP_PID"
    clear_app_turn
    watch=$(cat "$APP_WATCH" 2>/dev/null)
    # Stop the watcher and its children (lipc-wait-event and the loop that reads it), or they outlive it.
    [ -n "$watch" ] && kill_tree "$watch"
    rm -f "$APP_WATCH"
    if [ -f "$APP_RESTART" ]; then
        rm -f "$APP_RESTART"
        store_storage_clear
        log "app restarted after wake: $arg"
        cp "$APP_REQUEST" "$RUN/next" && run_app
        return
    fi
    store_storage_clear
    log "app exit: rc=$rc after $(( $(date +%s) - t0 ))s"
    # Only the installed Store may pass a request on exit 11. The launcher reads
    # the app ID, checks the install record, and uses the ordinary app route.
    if [ "$store_can_open" -eq 1 ] && [ "$rc" -eq 11 ] && [ -f "$STORE_OPEN" ]; then
        store_open_pending=yes
    else
        rm -f "$STORE_OPEN"
    fi
    rm -f "$STORE_OPEN.tmp"
    # Apps written for the stock UI may start it again on exit, or resume volumd.
    if framework_running; then
        log "the app left the framework running; stopping it again"
        framework_stop
    fi
    [ "$store_open_pending" = yes ] || store_storage_release
}

# Recover a Store left behind if this startup beats the previous supervisor's watchdog.
store_child_stop
logger_start
install_traps
# The power daemon must not prevent the screensaver, or the power button does not put the Kindle to sleep.
command -v lipc-set-prop >/dev/null 2>&1 && lipc-set-prop com.lab126.powerd preventScreenSaver 0 2>/dev/null
if framework_running; then
    framework_stop verify
else
    # Already stopped (an earlier run left it so): the marker makes leaving start it again, and the holder is what
    # keeps the launcher's sleep and wake working without the framework.
    log "framework not running; it will be started when the launcher leaves"
    echo stopped > "$MARK"
    holder_start verify
fi
[ -f "$UPDATE_JOURNAL" ] || volumd_resume
watchdog $$ &
mkdir -p "$LAUNCHER_USB"
rmdir "$LAUNCHER_USB/handling" 2>/dev/null
rm -f "$LAUNCHER_USB/held"
launcher_can_lease=0
[ -d "$LAUNCHER_USB" ] && [ -p "$LOG_OUT" ] && launcher_can_lease=1
usb_watch $$ &
USB_MONITOR=$!
trap 'log "supervisor exit (status $?)"' EXIT
# The system layer: the launcher asks for its app with SIGUSR1 and stays running (docs/system-architecture.md).
launch_requested=no
resident_app=no
trap 'launch_requested=yes' USR1
SUPERVISOR=$$
# The launcher's wrapper sends USR2 when the launcher ends, so an end while an app runs is seen at once.
launcher_ended=no
launcher_rc=
trap 'launcher_ended=yes' USR2

crashes=""
store_open_pending=no
refresh_mode=verify
# Only the initial launcher after the boot guard may draw a startup wallpaper.
startup_wallpaper=${KL_SHOW_STARTUP_WALLPAPER:-0}
unset KL_SHOW_STARTUP_WALLPAPER
while :; do
    update_begin || break
    if ! refresh_files "$refresh_mode"; then
        log "could not refresh the launcher binary from $DIR; giving up"
        launcher_cache_failed=yes
        break
    fi
    refresh_mode=
    if [ ! -x "$RUN/kl-launcher" ]; then
        log "no launcher binary in $DIR; giving up"
        break
    fi
    launch_started=$(date +%s)
    echo "$launch_started" > "$RUN/heartbeat"
    rm -f "$RUN/next" "$UPDATE_READY"
    start_launcher 0
    startup_wallpaper=0
    if [ "$store_open_pending" = yes ]; then
        # Open either exits directly into the app runner or reaches the launcher grid and its heartbeat.
        while store_child_alive "$lp"; do
            store_open_heartbeat=$(cat "$RUN/heartbeat" 2>/dev/null)
            if [ -n "$store_open_heartbeat" ] && [ "$store_open_heartbeat" != "$launch_started" ]; then
                store_storage_release
                store_open_pending=no
                break
            fi
            sleep 1
        done
    fi
    update_failed=no
    if [ "$trial" -eq 1 ]; then
        while kill -0 "$lp" 2>/dev/null; do
            if update_healthy; then
                if ! update_accept; then
                    update_failed=yes
                    launcher_signal KILL
                fi
                break
            fi
            sleep 1
        done
    fi
    # The launcher stays while its app runs: a launch request (SIGUSR1) interrupts the wait, the app runs, and the
    # wait goes on. A request that came before the wait started is handled first. The launcher's status comes from
    # the file its wrapper writes: a signal can make wait return early with a number that is not the status (busybox:
    # 128 + the signal number, for instance 140 for USR2).
    while :; do
        while [ "$launch_requested" != yes ] && [ ! -f "$LAUNCHER_STATUS" ] && kill -0 "$lp" 2>/dev/null; do
            wait "$lp" 2>/dev/null
        done
        launcher_ended=no
        if [ "$launch_requested" = yes ]; then
            launch_requested=no
            if kill -0 "$lp" 2>/dev/null; then
                resident_app=yes
                run_app
                resident_app=no
                # The Store asked for another app: the launcher starts again and opens it, as before.
                [ "$store_open_pending" = yes ] && [ -z "$launcher_rc" ] && launcher_signal TERM
                # The launcher ended while the app ran (the Kindle UI, or too many crashes): its status is kept.
                if [ -n "$launcher_rc" ]; then
                    break
                fi
                continue
            fi
        fi
        break
    done
    # Reap the wrapper (it may still be finishing), then take the launcher's status from the file it wrote.
    while kill -0 "$lp" 2>/dev/null; do
        wait "$lp" 2>/dev/null
    done
    if [ -n "$launcher_rc" ]; then
        rc=$launcher_rc
        launcher_rc=
    elif [ -f "$LAUNCHER_STATUS" ]; then
        rc=$(cat "$LAUNCHER_STATUS" 2>/dev/null)
    else
        rc=1
    fi
    launcher_proc=$(cat "$LAUNCHER_PID" 2>/dev/null)
    rm -f "$LAUNCHER_PID" "$LAUNCHER_STATUS" "$LAUNCHER_STATUS.tmp"
    launcher_storage_end
    if [ "$store_open_pending" = yes ]; then
        store_open_pending=no
        [ "$rc" -eq 10 ] || store_storage_release
    fi
    if [ "$trial" -eq 1 ]; then
        # A user may open an app or leave before the second heartbeat. The ready marker from this process
        # and its clean protocol exit prove this start; a crash or an old marker does not.
        if [ "$update_failed" = no ] && { [ "$rc" -eq 0 ] || [ "$rc" -eq 10 ]; } && update_ready; then
            update_accept || update_failed=yes
        fi
        if [ "$update_failed" = yes ]; then
            update_revert || break
            continue
        fi
        if [ "$trial" -eq 1 ]; then
            log "launcher update: trial exited before it was healthy (rc=$rc)"
            sleep 1
            continue
        fi
    fi
    case "$rc" in
        10)
            run_app
            ;;
        0)
            log "the Kindle UI was requested"
            break
            ;;
        143)
            log "launcher stopped (TERM); starting it again"
            ;;
        *)
            now=$(date +%s)
            recent=""
            for t in $crashes; do
                [ $((now - t)) -lt "$CRASH_WINDOW" ] && recent="$recent $t"
            done
            crashes="$recent $now"
            count=$(echo $crashes | wc -w)
            log "launcher crashed (rc=$rc), $count in ${CRASH_WINDOW}s"
            if [ "$count" -ge "$MAX_CRASHES" ]; then
                log "too many crashes; giving up"
                break
            fi
            sleep 1
            ;;
    esac
done

finish
exit 0
