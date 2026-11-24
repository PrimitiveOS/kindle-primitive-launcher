#!/bin/sh
# Runs deploy/supervisor.sh on the Mac against stub Kindle commands (start, stop, status, pidof, killall,
# usleep) and a fake launcher whose exit codes are scripted. Checks the exit-code protocol, the framework
# and volumd markers, the crash limit, the watchdog and the heartbeat.
#
#   sh tests/supervisor-test.sh
set -u

HERE=$(cd "$(dirname "$0")/.." && pwd)
SUP="$HERE/deploy/supervisor.sh"
PASS=0
FAIL=0

setup() {
    ROOT=$(mktemp -d /tmp/kl-sup-test.XXXXXX)
    export KL_DIR="$ROOT/dir" KL_RUN="$ROOT/run" KL_DOCUMENTS="$ROOT/documents" STATE="$ROOT/state"
    export KL_STORE_USB_PROC="$STATE/proc"
    mkdir -p "$KL_DIR/fonts" "$STATE" "$ROOT/bin" "$KL_DOCUMENTS" "$KL_STORE_USB_PROC/42"
    : > "$KL_DIR/fonts/NotoSans-Regular.ttf"
    : > "$KL_DIR/fonts/NotoSans-Bold.ttf"
    : > "$KL_DIR/holder.lua"
    echo running > "$STATE/fw"
    : > "$STATE/volumd"
    printf 'State:\tS (sleeping)\n' > "$KL_STORE_USB_PROC/42/status"
    printf '/dev/mmcblk0p4 /mnt/base-us vfat rw 0 0\nfsp /mnt/us fuse rw 0 0\n' > "$KL_STORE_USB_PROC/mounts"
    : > "$STATE/calls"
    cat > "$ROOT/bin/status" <<'EOF'
#!/bin/sh
if [ "$(cat "$STATE/fw")" = running ]; then echo "$1 start/running, process 99"; else echo "$1 stop/waiting"; fi
EOF
    cat > "$ROOT/bin/stop" <<'EOF'
#!/bin/sh
echo "stop $*" >> "$STATE/calls"; echo stopped > "$STATE/fw"
EOF
    cat > "$ROOT/bin/start" <<'EOF'
#!/bin/sh
echo "start $*" >> "$STATE/calls"; echo running > "$STATE/fw"
EOF
    cat > "$ROOT/bin/pidof" <<'EOF'
#!/bin/sh
case "$1" in
    volumd) [ -f "$STATE/volumd" ] && echo 42 ;;
    cvm) [ "$(cat "$STATE/fw")" = running ] ;;
    *) exit 1 ;;
esac
EOF
    cat > "$ROOT/bin/killall" <<'EOF'
#!/bin/sh
echo "killall $*" >> "$STATE/calls"
case "$*" in
    '-STOP volumd')
        if [ ! -f "$STATE/volumd-wont-stop" ]; then printf 'State:\tT (stopped)\n' > "$KL_STORE_USB_PROC/42/status"; fi
        ;;
    '-CONT volumd') printf 'State:\tS (sleeping)\n' > "$KL_STORE_USB_PROC/42/status" ;;
esac
EOF
    cat > "$ROOT/bin/lipc-set-prop" <<'EOF'
#!/bin/sh
echo "lipc-set-prop $*" >> "$STATE/calls"
EOF
    # lipc-daemon: answers usid unless the test marks it wedged; "restart lipcd" clears that.
    cat > "$ROOT/bin/lipc-get-prop" <<'EOF'
#!/bin/sh
[ -f "$STATE/lipcd-wedged" ] && exit 1
echo G000TEST
EOF
    cat > "$ROOT/bin/restart" <<'EOF'
#!/bin/sh
echo "restart $*" >> "$STATE/calls"
[ "$1" = lipcd ] && rm -f "$STATE/lipcd-wedged"
EOF
    cat > "$ROOT/bin/usleep" <<'EOF'
#!/bin/sh
exit 0
EOF
    # The fake launcher: takes the next scripted code from $STATE/codes.
    cat > "$KL_DIR/kl-launcher" <<'EOF'
#!/bin/sh
if [ "$1" = --wait-screen ]; then echo "wait screen shown" >> "$STATE/calls"; exec sleep 1000; fi
code=$(head -n 1 "$STATE/codes")
tail -n +2 "$STATE/codes" > "$STATE/codes.tmp"; mv "$STATE/codes.tmp" "$STATE/codes"
echo "launcher run: $code" >> "$STATE/calls"
echo "launcher says hello $code"
[ -p /dev/fd/1 ] && echo "launcher stdout is a pipe" >> "$STATE/calls"
case "$code" in
    settings-write)
        [ "$KL_LAUNCHER_USB_LEASE" = 1 ] || exit 3
        mailbox=$KL_RUN/launcher-usb
        # Ask for the same write/idle sequence used by Launcher Settings.
        printf '%s 1 write\n' "$$" > "$mailbox/store-storage-request.tmp"
        mv "$mailbox/store-storage-request.tmp" "$mailbox/store-storage-request"
        for i in 1 2 3 4 5; do
            [ "$(cat "$mailbox/store-storage-reply" 2>/dev/null)" != "$$ 1 paused" ] || break
            sleep 1
        done
        [ "$(cat "$mailbox/store-storage-reply" 2>/dev/null)" = "$$ 1 paused" ] || exit 3
        grep -q 'State:.*T (stopped)' "$KL_STORE_USB_PROC/42/status" || exit 3
        echo "settings write had the lease" >> "$STATE/calls"
        printf '%s 2 idle\n' "$$" > "$mailbox/store-storage-request.tmp"
        mv "$mailbox/store-storage-request.tmp" "$mailbox/store-storage-request"
        for i in 1 2 3 4 5; do
            if grep -q 'State:.*S (sleeping)' "$KL_STORE_USB_PROC/42/status"; then
                echo "settings released the lease while open" >> "$STATE/calls"
                exit 0
            fi
            sleep 1
        done
        exit 3
        ;;
    10) printf '%s\nscript\n%s\n' "$STATE" "$STATE/app.sh" > "$KL_RUN/next"; exit 10 ;;
    store) printf '%s\nscript\n%s\n' "$KL_DOCUMENTS" "$KL_DOCUMENTS/Store.sh" > "$KL_RUN/next"; exit 10 ;;
    store-installed) printf '%s\nscript\n%s\ninstalled\n' "$KL_DOCUMENTS" "$KL_DOCUMENTS/Store.sh" > "$KL_RUN/next"; exit 10 ;;
    other-installed) printf '%s\nscript\n%s\ninstalled\n' "$STATE" "$STATE/app.sh" > "$KL_RUN/next"; exit 10 ;;
    native) printf '%s\nscript\n%s\n' "$KL_DOCUMENTS" "$KL_DOCUMENTS/Native App.sh" > "$KL_RUN/next"; exit 10 ;;
    open-request)
        if [ -f "$KL_RUN/store-open" ]; then
            printf 'store request: %s\n' "$(cat "$KL_RUN/store-open")" >> "$STATE/calls"
            grep -q 'State:.*T (stopped)' "$KL_STORE_USB_PROC/42/status" && echo "store handoff stayed paused" >> "$STATE/calls"
            rm -f "$KL_RUN/store-open"
            printf '%s\nscript\n%s\n' "$KL_DOCUMENTS" "$KL_DOCUMENTS/Example.sh" > "$KL_RUN/next"
            exit 10
        fi
        exit 0
        ;;
    open-refused)
        rm -f "$KL_RUN/store-open"
        echo "$(( $(date +%s) + 1 ))" > "$KL_RUN/heartbeat"
        for i in 1 2 3 4 5 6; do
            if grep -q 'State:.*S (sleeping)' "$KL_STORE_USB_PROC/42/status"; then
                echo "launcher grid restored USB" >> "$STATE/calls"
                exit 0
            fi
            sleep 1
        done
        exit 3
        ;;
    # The system layer: ask the supervisor for the slow app and stay; then freeze it (quick settings) and crash, or
    # leave for the Kindle UI over it.
    resident-crash|resident-ui|resident-native-crash)
        if [ "$code" = resident-native-crash ]; then
            printf '%s\nscript\n%s\n' "$KL_DOCUMENTS" "$KL_DOCUMENTS/Slow Native.sh" > "$KL_RUN/next"
        else
            printf '%s\nscript\n%s\n' "$STATE" "$STATE/slow-app.sh" > "$KL_RUN/next"
        fi
        kill -USR1 "$(cat "$KL_RUN/supervisor.pid")"
        for i in 1 2 3 4 5 6 7 8 9 10; do [ -f "$KL_RUN/app.pid" ] && break; sleep 1; done
        sleep 1
        [ "$code" = resident-ui ] && exit 0
        kill -STOP "$(cat "$KL_RUN/app.pid")"
        exit 3
        ;;
    attach)
        echo "launcher attach: KL_ATTACH_APP=$KL_ATTACH_APP" >> "$STATE/calls"
        while [ -f "$KL_RUN/app.pid" ]; do sleep 1; done
        exit 0
        ;;
    usr2-interrupt)
        # The signal the wrapper sends when a launcher ends can interrupt the supervisor's wait while this one still runs.
        kill -USR2 "$(cat "$KL_RUN/supervisor.pid")"
        sleep 2
        exit 0
        ;;
    term) kill -TERM $$ ;;
    hang) while :; do sleep 1; done ;;
    "") exit 0 ;;
    *) exit "$code" ;;
esac
EOF
    cat > "$STATE/app.sh" <<'EOF'
echo "app ran: PRIMITIVE_LAUNCHER=$PRIMITIVE_LAUNCHER pwd=$(pwd)" >> "$STATE/calls"
[ -f "$STATE/app-starts-framework" ] && start lab126_gui
exit 3
EOF
    cat > "$STATE/slow-app.sh" <<'EOF'
echo "slow app started" >> "$STATE/calls"
trap 'echo "slow app got TERM" >> "$STATE/calls"; exit 0' TERM
i=0
while [ $i -lt 6 ]; do sleep 1; i=$((i + 1)); done
echo "slow app ended" >> "$STATE/calls"
EOF
    # The holder: a stub lua that records its start and sleeps (exec, so the pid is the sleep's pid). With
    # $STATE/holder-ignores-term it ignores TERM, as when it inherited an ignored TERM.
    cat > "$ROOT/bin/lua" <<'EOF'
#!/bin/sh
echo "holder run: $*" >> "$STATE/calls"
if [ -s "$1" ]; then cat "$1" >> "$STATE/calls"; fi
if [ -f "$STATE/holder-ignores-term" ]; then
    trap '' TERM
    while :; do sleep 1; done
fi
exec sleep 1000
EOF
    # lipc-probe: the library app's lipc service exists once the framework runs and the app has registered
    # ($STATE/registered-after holds how many probes to fail first).
    cat > "$ROOT/bin/lipc-probe" <<'EOF'
#!/bin/sh
# "lipc-probe com.lab126.KPPMainApp" lists the library app's properties once it is ready.
n=$(cat "$STATE/registered-after" 2>/dev/null || echo 0)
if [ "$n" -gt 0 ]; then echo $((n - 1)) > "$STATE/registered-after"; printf '\tw\tStr\tpause\n'; exit 0; fi
[ "$(cat "$STATE/fw")" = running ] && printf '\tw\tStr\tpause\n\tw\tStr\tload\n\tw\tStr\tgo\n'
exit 0
EOF
    # initctl: the library app is running once the framework has been started.
    cat > "$ROOT/bin/initctl" <<'EOF'
#!/bin/sh
if [ "$1 $2" = "status kppmainapp" ] && [ "$(cat "$STATE/fw")" = running ]; then echo "kppmainapp start/running, process 7"; else echo "$2 stop/waiting"; fi
EOF
    chmod 755 "$ROOT/bin"/* "$KL_DIR/kl-launcher"
    export PATH="$ROOT/bin:$PATH"
}

teardown() {
    rm -rf "$ROOT"
}

check() {
    if eval "$2"; then
        PASS=$((PASS + 1))
    else
        FAIL=$((FAIL + 1))
        echo "FAIL: $1"
        echo "--- calls"; cat "$STATE/calls"; echo "--- log"; cat "$KL_DIR/log-launcher.txt" 2>/dev/null
    fi
}

run_sup() {
    # A time limit, so a broken supervisor cannot hang the test.
    sh "$SUP" &
    sp=$!
    i=0
    while kill -0 "$sp" 2>/dev/null && [ $i -lt 30 ]; do sleep 1; i=$((i + 1)); done
    kill -0 "$sp" 2>/dev/null && { echo "supervisor did not end"; kill -9 "$sp"; }
    sleep 1
}

echo "test: a cold supervisor uses the installed bytes even when the cached files are newer"
setup
mkdir -p "$KL_RUN/fonts"
cat > "$KL_RUN/kl-launcher" <<'EOF'
#!/bin/sh
echo "stale launcher ran" >> "$STATE/calls"
EOF
chmod 755 "$KL_RUN/kl-launcher"
printf 'stale holder\n' > "$KL_RUN/holder.lua"
printf 'stale font\n' > "$KL_RUN/fonts/NotoSans-Regular.ttf"
printf 'installed holder\n' > "$KL_DIR/holder.lua"
printf 'installed font\n' > "$KL_DIR/fonts/NotoSans-Regular.ttf"
touch -t 200001010000 "$KL_DIR/kl-launcher" "$KL_DIR/holder.lua" "$KL_DIR/fonts/NotoSans-Regular.ttf"
printf '10\n0\n' > "$STATE/codes"
run_sup
check "installed launcher runs both before and after the app" '[ "$(grep -c "launcher run:" "$STATE/calls")" = 2 ]'
check "the older installed binary replaces the stale one" 'cmp -s "$KL_DIR/kl-launcher" "$KL_RUN/kl-launcher"'
check "the correct holder starts before the launcher" 'grep -qx "installed holder" "$STATE/calls" && ! grep -qx "stale holder" "$STATE/calls"'
check "the older installed font replaces the stale one" 'cmp -s "$KL_DIR/fonts/NotoSans-Regular.ttf" "$KL_RUN/fonts/NotoSans-Regular.ttf"'
check "the launcher is copied only once in the session" '[ "$(grep -c "copied kl-launcher to" "$KL_DIR/log-launcher.txt")" = 1 ]'
teardown

echo "test: a failed cold refresh cannot run the stale launcher or wait screen"
setup
mkdir -p "$KL_RUN/kl-launcher.new"
cat > "$KL_RUN/kl-launcher" <<'EOF'
#!/bin/sh
echo "stale launcher ran" >> "$STATE/calls"
EOF
chmod 755 "$KL_RUN/kl-launcher"
touch -t 200001010000 "$KL_DIR/kl-launcher"
run_sup
check "stale binary did not run" '! grep -q "stale launcher ran" "$STATE/calls"'
check "refresh failure logged" 'grep -q "could not refresh the launcher binary" "$KL_DIR/log-launcher.txt"'
check "framework restored without running the stale wait screen" '[ "$(cat "$STATE/fw")" = running ]'
teardown

echo "test: launch an app, then ask for the Kindle UI"
setup
printf '10\n0\n' > "$STATE/codes"
run_sup
check "framework stopped first" 'grep -v "^lipc-set-prop" "$STATE/calls" | head -n 1 | grep -q "stop lab126_gui"'
check "power daemon does not prevent the screensaver" 'grep -q "^lipc-set-prop com.lab126.powerd preventScreenSaver 0" "$STATE/calls"'
check "volumd paused" 'grep -q "killall -STOP volumd" "$STATE/calls"'
check "app ran with the env" 'grep -q "app ran: PRIMITIVE_LAUNCHER=1 pwd=$STATE" "$STATE/calls"'
check "launcher ran twice" '[ "$(grep -c "launcher run" "$STATE/calls")" = 2 ]'
check "framework running at the end" '[ "$(cat "$STATE/fw")" = running ]'
check "volumd resumed" 'grep -q "killall -CONT volumd" "$STATE/calls"'
check "markers removed" '[ ! -f "$KL_RUN/framework-stopped" ] && [ ! -f "$KL_RUN/volumd-paused" ] && [ ! -f "$KL_RUN/supervisor.pid" ]'
check "binary copied to run" '[ -x "$KL_RUN/kl-launcher" ]'
teardown

echo "test: each app run gets its own foreground turn file"
setup
export REAL_AWK=$(command -v awk)
cat > "$ROOT/bin/awk" <<'EOF'
#!/bin/sh
case "${2:-}" in
    /proc/[0-9]*/stat) printf '75\n' ;;
    *) exec "$REAL_AWK" "$@" ;;
esac
EOF
chmod 755 "$ROOT/bin/awk"
cat > "$STATE/app.sh" <<'EOF'
case "$KL_APP_TURN_FILE" in
    "$KL_RUN"/app-turn.??????/turn) ;;
    *) exit 1 ;;
esac
printf '%s\n' "$KL_APP_TURN_FILE" >> "$STATE/turn-files"
printf 'v1 D fixed\n' > "$KL_APP_TURN_FILE.tmp" && mv "$KL_APP_TURN_FILE.tmp" "$KL_APP_TURN_FILE"
sleep 1
dir=${KL_APP_TURN_FILE%/turn}
printf 'v1 %s 75 %s\n' "$(cat "$KL_RUN/app.pid")" "${dir##*/}" > "$STATE/expected-owner"
cmp -s "$STATE/expected-owner" "$KL_RUN/app-turn-owner" && echo "foreground owner published" >> "$STATE/calls"
EOF
printf '10\n10\n0\n' > "$STATE/codes"
run_sup
check "each app was given a unique turn file" '[ "$(sort -u "$STATE/turn-files" | wc -l | tr -d " ")" = 2 ]'
check "each turn file had a live supervisor owner record" '[ "$(grep -c "foreground owner published" "$STATE/calls")" = 2 ]'
check "exiting apps cleared their turn files and ownership" '! find "$KL_RUN" -name "app-turn*" -print | grep -q .'
teardown

echo "test: settings writes hold a short storage lease while Launcher stays open"
setup
printf 'settings-write\n' > "$STATE/codes"
run_sup
check "Launcher used the existing storage handshake" 'grep -qx "settings write had the lease" "$STATE/calls"'
check "volumd resumed before Launcher exited" 'grep -qx "settings released the lease while open" "$STATE/calls"'
check "no stale lease records" '[ ! -f "$KL_RUN/launcher-usb/held" ] && [ ! -f "$KL_RUN/launcher-usb/store-storage-request" ]'
teardown

echo "test: the holder runs while the framework is stopped, and stops before the UI returns"
setup
printf '10\n0\n' > "$STATE/codes"
run_sup
check "holder started" 'grep -q "holder run" "$STATE/calls"'
check "holder stopped" 'grep -q "holder stopped" "$KL_DIR/log-launcher.txt"'
check "holder pid removed" '[ ! -f "$KL_RUN/holder.pid" ]'
check "holder copied to run" '[ -f "$KL_RUN/holder.lua" ]'
teardown

echo "test: a wedged lipc-daemon is restarted before the framework starts"
setup
: > "$STATE/lipcd-wedged"
printf '0\n' > "$STATE/codes"
run_sup
check "lipcd restarted" 'grep -q "restart lipcd" "$STATE/calls"'
check "logged and recovered" 'grep -q "lipc-daemon answers again" "$KL_DIR/log-launcher.txt"'
check "restart before the framework start" '[ "$(grep -n "restart lipcd" "$STATE/calls" | cut -d: -f1)" -lt "$(grep -n "start lab126_gui" "$STATE/calls" | cut -d: -f1)" ]'
teardown

echo "test: a healthy lipc-daemon is left alone"
setup
printf '0\n' > "$STATE/codes"
run_sup
check "no lipcd restart" '! grep -q "restart lipcd" "$STATE/calls"'
teardown

echo "test: a holder that ignores TERM is killed before the framework starts"
setup
: > "$STATE/holder-ignores-term"
printf '0\n' > "$STATE/codes"
run_sup
check "killed with KILL" 'grep -q "holder did not stop on TERM; killed" "$KL_DIR/log-launcher.txt"'
check "no holder left" '! pgrep -f "$KL_RUN/holder.lua" >/dev/null'
teardown

echo "test: the supervisor waits until the library app has registered"
setup
printf '0\n' > "$STATE/codes"
echo 3 > "$STATE/registered-after"
run_sup
check "ready logged once the library offers load" 'grep -q "the Kindle UI is ready after 3s" "$KL_DIR/log-launcher.txt"'
teardown

echo "test: the waiting screen covers the Kindle UI start and is stopped after"
setup
printf '0\n' > "$STATE/codes"
run_sup
check "shown" 'grep -q "wait screen shown" "$STATE/calls"'
check "stopped" 'grep -q "wait screen stopped" "$KL_DIR/log-launcher.txt"'
check "stopped after the Kindle UI was ready" '[ "$(grep -n "wait screen stopped" "$KL_DIR/log-launcher.txt" | cut -d: -f1)" -gt "$(grep -n "the Kindle UI is ready" "$KL_DIR/log-launcher.txt" | cut -d: -f1)" ]'
check "no wait pid left" '[ ! -f "$KL_RUN/wait.pid" ]'
teardown

echo "test: three crashes give up and restore"
setup
printf '1\n1\n1\n1\n' > "$STATE/codes"
run_sup
check "three runs" '[ "$(grep -c "launcher run" "$STATE/calls")" = 3 ]'
check "gave up" 'grep -q "too many crashes" "$KL_DIR/log-launcher.txt"'
check "framework restored" '[ "$(cat "$STATE/fw")" = running ]'
teardown

echo "test: a launcher crash over a frozen app starts the launcher again over the app"
setup
printf 'resident-crash\nattach\n' > "$STATE/codes"
run_sup
check "the app ran once" '[ "$(grep -c "slow app started" "$STATE/calls")" = 1 ]'
check "the frozen app was resumed and finished" 'grep -qx "slow app ended" "$STATE/calls"'
check "the new launcher follows the app" 'grep -qx "launcher attach: KL_ATTACH_APP=1" "$STATE/calls"'
check "the restart is logged" 'grep -q "the launcher started again over the app" "$KL_DIR/log-launcher.txt"'
check "framework running at the end" '[ "$(cat "$STATE/fw")" = running ]'
teardown

echo "test: a launcher crash over an app with a storage lease starts the launcher again"
setup
cat > "$ROOT/bin/usleep" <<'EOF'
#!/bin/sh
if [ "$1" -eq 100000 ]; then sleep 0.05; fi
EOF
cat > "$KL_DOCUMENTS/Slow Native.sh" <<'EOF'
#!/bin/sh
# PrimitiveUSBLease
echo "slow native started" >> "$STATE/calls"
i=0
while [ $i -lt 6 ]; do sleep 1; i=$((i + 1)); done
echo "slow native ended" >> "$STATE/calls"
EOF
printf 'resident-native-crash\nattach\n' > "$STATE/codes"
run_sup
check "the leased app ran once and finished" '[ "$(grep -c "slow native started" "$STATE/calls")" = 1 ] && grep -qx "slow native ended" "$STATE/calls"'
check "the new launcher follows the leased app" 'grep -qx "launcher attach: KL_ATTACH_APP=1" "$STATE/calls"'
teardown

echo "test: the Kindle UI asked over an app closes the app first"
setup
printf 'resident-ui\n' > "$STATE/codes"
run_sup
check "the app was closed" 'grep -qx "slow app got TERM" "$STATE/calls" && ! grep -qx "slow app ended" "$STATE/calls"'
check "the launcher ran once" '[ "$(grep -c "launcher run" "$STATE/calls")" = 1 ]'
check "the request is logged" 'grep -q "the Kindle UI was requested over the app" "$KL_DIR/log-launcher.txt"'
check "framework running at the end" '[ "$(cat "$STATE/fw")" = running ]'
teardown

echo "test: a signal that interrupts the supervisor's wait is not taken for the launcher's exit status"
setup
printf 'usr2-interrupt\n' > "$STATE/codes"
run_sup
check "one launcher run" '[ "$(grep -c "launcher run" "$STATE/calls")" = 1 ]'
check "the Kindle UI was requested normally" 'grep -q "the Kindle UI was requested" "$KL_DIR/log-launcher.txt"'
check "no crash was logged" '! grep -q "launcher crashed" "$KL_DIR/log-launcher.txt"'
teardown

echo "test: TERM restarts without counting as a crash"
setup
printf 'term\nterm\nterm\n0\n' > "$STATE/codes"
run_sup
check "four runs" '[ "$(grep -c "launcher run" "$STATE/calls")" = 4 ]'
check "no give up" '! grep -q "too many crashes" "$KL_DIR/log-launcher.txt"'
teardown

echo "test: an app that starts the framework is undone"
setup
touch "$STATE/app-starts-framework"
printf '10\n0\n' > "$STATE/codes"
run_sup
check "noticed" 'grep -q "left the framework running" "$KL_DIR/log-launcher.txt"'
check "stopped twice" '[ "$(grep -c "stop lab126_gui" "$STATE/calls")" = 2 ]'
teardown

echo "test: the watchdog restores the UI when the supervisor is killed"
setup
export KL_WATCH_EVERY=1
printf 'hang\n' > "$STATE/codes"
sh "$SUP" &
sp=$!
sleep 2
check "framework stopped while running" '[ "$(cat "$STATE/fw")" = stopped ]'
kill -9 "$sp"
sleep 3
check "watchdog restored" '[ "$(cat "$STATE/fw")" = running ]'
check "watchdog logged" 'grep -q "supervisor gone" "$KL_DIR/log-launcher.txt"'
check "watchdog released the holder" 'grep -q "holder stopped" "$KL_DIR/log-launcher.txt"'
pkill -f "$KL_RUN/kl-launcher" 2>/dev/null
teardown

echo "test: a launcher without heartbeat is killed and restarted"
setup
export KL_WATCH_EVERY=1 KL_HEARTBEAT_LIMIT=1
printf 'hang\n0\n' > "$STATE/codes"
run_sup
check "killed by the watchdog" 'grep -q "no heartbeat" "$KL_DIR/log-launcher.txt"'
check "restarted and ended" '[ "$(grep -c "launcher run" "$STATE/calls")" = 2 ] && [ "$(cat "$STATE/fw")" = running ]'
unset KL_WATCH_EVERY KL_HEARTBEAT_LIMIT
teardown

echo "test: a second start does nothing"
setup
printf 'hang\n' > "$STATE/codes"
mkdir -p "$KL_RUN"
sleep 100 &
holder=$!
echo "$holder" > "$KL_RUN/supervisor.pid"
sh "$SUP"
check "already running" 'grep -q "already running" "$KL_DIR/log-launcher.txt"'
check "framework untouched" '[ "$(cat "$STATE/fw")" = running ] && [ ! -s "$STATE/calls" ]'
kill "$holder"
teardown

echo "test: a launcher started with the framework down brings it back when it leaves"
setup
echo stopped > "$STATE/fw"
printf '0\n' > "$STATE/codes"
run_sup
check "framework started on leaving" '[ "$(cat "$STATE/fw")" = running ]'
check "framework was not stopped by the supervisor" '! grep -q "stop lab126_gui" "$STATE/calls"'
teardown

echo "test: volumd keeps running while the launcher is in front"
setup
printf '0\n' > "$STATE/codes"
run_sup
check "volumd never paused without an app" '! grep -q "killall -STOP volumd" "$STATE/calls"'
check "volumd running at the end" 'grep -q "killall -CONT volumd" "$STATE/calls"'
teardown

echo "test: the launcher logs through a pipe, not an open file on the user storage"
setup
printf '0\n' > "$STATE/codes"
run_sup
check "output reached the log" 'grep -q "launcher says hello 0" "$KL_DIR/log-launcher.txt"'
check "stdout was a pipe" 'grep -q "launcher stdout is a pipe" "$STATE/calls"'
check "no pipe left behind" '[ ! -p "$KL_RUN/log.fifo" ]'
teardown

setup_update() {
    setup
    export KL_UPDATE_HEALTHY_SECS=0
    mkdir -p "$KL_DIR/store" "$KL_RUN"
    printf '0\n' > "$STATE/codes"
    : > "$KL_DIR/store/journal.json"
    echo 0 > "$STATE/attempts"
    mv "$KL_DIR/kl-launcher" "$STATE/ordinary-launcher"
    cat > "$KL_DIR/kl-launcher" <<'OLD'
#!/bin/sh
if [ "$1" != --launcher-update ]; then exec "$STATE/ordinary-launcher" "$@"; fi
old="$KL_DIR/kl-launcher.prev"
pending="$KL_DIR/store/journal.json"
echo "driver $2" >> "$STATE/calls"
case "$2" in
    begin)
        [ -f "$pending" ] || { echo idle; exit 0; }
        n=$(cat "$STATE/attempts")
        if [ "$n" -ge 3 ]; then
            mv "$old" "$KL_DIR/kl-launcher"
            rm -f "$pending"
            echo reverted
            exit 0
        fi
        if [ ! -f "$old" ]; then
            cp "$KL_DIR/kl-launcher" "$old"
            cp "$STATE/candidate" "$KL_DIR/kl-launcher"
        fi
        echo $((n + 1)) > "$STATE/attempts"
        echo trial
        ;;
    accept)
        [ -f "$STATE/mirrored" ] || exit 1
        rm -f "$old" "$pending"
        : > "$STATE/accepted"
        ;;
    revert)
        mv "$old" "$KL_DIR/kl-launcher"
        rm -f "$pending"
        ;;
    *) exit 1 ;;
esac
OLD
    cat > "$KL_DIR/setup-boot.sh" <<'MIRROR'
#!/bin/sh
[ "$KL_SETUP_BOOT_STRICT" = 1 ] || exit 1
if cmp -s "$KL_DIR/kl-launcher" "$STATE/candidate"; then
    echo "candidate mirror" >> "$STATE/calls"
    [ ! -f "$STATE/fail-mirror" ] || exit 1
else
    echo "previous mirror" >> "$STATE/calls"
fi
: > "$STATE/mirrored"
MIRROR
    cat > "$STATE/candidate" <<'CANDIDATE'
#!/bin/sh
echo "candidate trial=$KL_LAUNCHER_UPDATE_TRIAL" >> "$STATE/calls"
[ "$KL_LAUNCHER_UPDATE_TRIAL" = 1 ] || exit 2
if [ -f "$STATE/stale-ready" ]; then
    echo 999999 > "$KL_RUN/launcher-update-ready"
    exit 0
fi
echo "$$" > "$KL_RUN/launcher-update-ready"
sleep 1
date +%s > "$KL_RUN/heartbeat"
count=0
while [ "$count" -lt 7 ]; do
    [ ! -f "$STATE/accepted" ] || exit 0
    count=$((count + 1))
    sleep 1
done
exit 2
CANDIDATE
    chmod 755 "$KL_DIR/kl-launcher" "$STATE/candidate"
}

echo "test: a candidate proves it can start before backups are removed"
setup_update
run_sup
check "candidate ran with the trial flag" 'grep -q "candidate trial=1" "$STATE/calls"'
check "accepted before the process exited" '[ -f "$STATE/accepted" ]'
check "candidate boot copy refreshed" 'grep -q "candidate mirror" "$STATE/calls"'
check "journal and temp driver gone" '[ ! -f "$KL_DIR/store/journal.json" ] && [ ! -f "$KL_RUN/launcher-update-driver" ]'
check "candidate still installed" 'cmp -s "$KL_DIR/kl-launcher" "$STATE/candidate"'
check "framework and USB state restored" '[ "$(cat "$STATE/fw")" = running ] && [ ! -f "$KL_RUN/volumd-paused" ]'
teardown

echo "test: an old readiness marker cannot accept the candidate and retry limit restores the launcher"
setup_update
touch "$STATE/stale-ready"
run_sup
check "exactly three candidate starts" '[ "$(grep -c "candidate trial=1" "$STATE/calls")" = 3 ]'
check "candidate was never accepted" '[ ! -f "$STATE/accepted" ]'
check "old mirror and binary returned" 'grep -q "previous mirror" "$STATE/calls" && grep -q "launcher run: 0" "$STATE/calls"'
check "reported restoration" 'grep -q "previous files restored after failed starts" "$KL_DIR/log-launcher.txt"'
teardown

echo "test: a cold start uses the old driver for a previously started trial"
setup_update
touch "$STATE/stale-ready"
cp "$KL_DIR/kl-launcher" "$KL_DIR/kl-launcher.prev"
cp "$STATE/candidate" "$KL_DIR/kl-launcher"
echo 2 > "$STATE/attempts"
run_sup
check "only the persisted last attempt was run" '[ "$(grep -c "candidate trial=1" "$STATE/calls")" = 1 ]'
check "restored launcher started" 'grep -q "launcher run: 0" "$STATE/calls"'
teardown

echo "test: a boot mirror failure restores the earlier launcher"
setup_update
touch "$STATE/fail-mirror"
run_sup
check "rollback ran" 'grep -q "driver revert" "$STATE/calls"'
check "old mirror and launcher restored" 'grep -q "previous mirror" "$STATE/calls" && grep -q "launcher run: 0" "$STATE/calls"'
check "failed mirror never accepted" '[ ! -f "$STATE/accepted" ]'
teardown

echo "test: only a Store launch can request opening another installed app"
setup
cat > "$KL_DOCUMENTS/Store.sh" <<'EOF'
[ "$KL_STORE_OPEN_APP" = 1 ] || exit 3
printf %s example-app > "$KL_RUN/store-open"
exit 11
EOF
cat > "$KL_DOCUMENTS/Example.sh" <<'EOF'
[ "$KL_STORE_OPEN_APP" = 0 ] && [ "$KL_STORE_USB_LEASE" = 0 ] || exit 3
echo "example app opened" >> "$STATE/calls"
EOF
printf 'store\nopen-request\n0\n' > "$STATE/codes"
run_sup
check "the Store request reaches the new launcher process" 'grep -q "store request: example-app" "$STATE/calls"'
check "the Store request keeps USB paused during handoff" 'grep -q "store handoff stayed paused" "$STATE/calls"'
check "the requested app runs as an ordinary app" 'grep -q "example app opened" "$STATE/calls"'
check "the request is consumed" '[ ! -f "$KL_RUN/store-open" ]'
teardown

echo "test: the launcher grid restores USB when a Store Open request is refused"
setup
cat > "$KL_DOCUMENTS/Store.sh" <<'EOF'
printf %s missing-app > "$KL_RUN/store-open"
exit 11
EOF
printf 'store\nopen-refused\n' > "$STATE/codes"
run_sup
check "a fresh grid heartbeat releases the inherited pause" 'grep -q "launcher grid restored USB" "$STATE/calls"'
teardown

echo "test: Apps settings passes the Installed destination only to its Store launch"
setup
cat > "$KL_DOCUMENTS/Store.sh" <<'EOF'
printf 'store destination [%s]\n' "$KL_STORE_START_TAB" >> "$STATE/calls"
EOF
cat > "$STATE/app.sh" <<'EOF'
printf 'other destination [%s]\n' "$KL_STORE_START_TAB" >> "$STATE/calls"
EOF
printf 'store-installed\nstore\nother-installed\n0\n' > "$STATE/codes"
run_sup
check "canonical Store receives the installed destination" 'grep -Fqx "store destination [installed]" "$STATE/calls"'
check "ordinary Store launch still starts without a destination" 'grep -Fqx "store destination []" "$STATE/calls"'
check "another app receives no Store destination" 'grep -Fqx "other destination []" "$STATE/calls"'
teardown

echo "test: another app's Store request is cleared"
setup
cat > "$STATE/app.sh" <<'EOF'
[ "$KL_STORE_OPEN_APP" = 0 ] || exit 3
printf %s example-app > "$KL_RUN/store-open"
exit 11
EOF
printf '10\nopen-request\n0\n' > "$STATE/codes"
run_sup
check "the unrelated app request is discarded" '! grep -q "store request:" "$STATE/calls"'
check "there is no stale request file" '[ ! -f "$KL_RUN/store-open" ]'
teardown

echo "test: idle Store allows USB and only a mounted, paused volume grants write access"
setup
cat > "$KL_DOCUMENTS/Store.sh" <<'EOF'
[ "$KL_STORE_USB_LEASE" = 1 ] || exit 3
cd / || exit 3
[ -p /dev/fd/1 ] && echo "store stdout is a pipe" >> "$STATE/calls"
daemon_state() { awk '$1 == "State:" {print $2}' "$KL_STORE_USB_PROC/42/status"; }
request() {
    printf '%s %s %s\n' "$1" "$2" "$3" > "$KL_RUN/store-storage-request.tmp"
    mv "$KL_RUN/store-storage-request.tmp" "$KL_RUN/store-storage-request"
}
reply() { cat "$KL_RUN/store-storage-reply" 2>/dev/null; }
until_matches() {
    count=0
    while [ "$("$1")" != "$2" ]; do
        [ "$count" -lt 6 ] || exit 4
        sleep 1
        count=$((count + 1))
    done
}
request 999999 1 idle
sleep 2
[ "$(daemon_state)" = T ] && echo "foreign request ignored" >> "$STATE/calls"
request $$ 1 idle
until_matches daemon_state S
echo "store idle permits USB" >> "$STATE/calls"
cp "$KL_STORE_USB_PROC/mounts" "$STATE/mounts.saved"
printf 'fsp /mnt/us fuse rw 0 0\n' > "$KL_STORE_USB_PROC/mounts"
request $$ 2 write
until_matches reply "$$ 2 unavailable"
[ "$(daemon_state)" = S ] && echo "USB drive stayed attached" >> "$STATE/calls"
cp "$STATE/mounts.saved" "$KL_STORE_USB_PROC/mounts"
: > "$STATE/volumd-wont-stop"
request $$ 3 write
until_matches reply "$$ 3 unavailable"
echo "running daemon denied" >> "$STATE/calls"
rm "$STATE/volumd-wont-stop"
request $$ 4 write
until_matches reply "$$ 4 paused"
[ "$(daemon_state)" = T ] && echo "store wrote with USB disabled" >> "$STATE/calls"
request $$ 5 idle
until_matches daemon_state S
echo "store restored idle USB" >> "$STATE/calls"
EOF
printf 'store\n0\n' > "$STATE/codes"
run_sup
check "leased Store output does not hold the flash log" 'grep -q "store stdout is a pipe" "$STATE/calls"'
check "stale process request is ignored" 'grep -q "foreign request ignored" "$STATE/calls"'
check "idle Store allows the drive" 'grep -q "store idle permits USB" "$STATE/calls"'
check "already ejected storage denies writes" 'grep -q "USB drive stayed attached" "$STATE/calls"'
check "write is denied if volumd cannot be stopped" 'grep -q "running daemon denied" "$STATE/calls"'
check "write is granted only with volumd stopped" 'grep -q "store wrote with USB disabled" "$STATE/calls"'
check "USB works after the write" 'grep -q "store restored idle USB" "$STATE/calls"'
check "Store lease files are removed on exit" '[ ! -e "$KL_RUN/store-storage-request" ] && [ ! -e "$KL_RUN/store-storage-reply" ] && [ ! -e "$KL_RUN/store-child.pid" ]'
teardown

echo "test: a legacy Store remains protected until exit"
setup
cat > "$KL_DOCUMENTS/Store.sh" <<'EOF'
sleep 2
if grep -q 'State:.*T (stopped)' "$KL_STORE_USB_PROC/42/status"; then
    echo "legacy Store stayed paused" >> "$STATE/calls"
fi
EOF
printf 'store\n0\n' > "$STATE/codes"
run_sup
check "legacy Store retains the startup pause" 'grep -q "legacy Store stayed paused" "$STATE/calls"'
check "legacy Store exit restores volumd" 'grep -q "State:.*S (sleeping)" "$KL_STORE_USB_PROC/42/status"'
teardown

echo "test: the watchdog stops a Store with write access before restoring USB"
setup
cat > "$KL_DOCUMENTS/Store.sh" <<'EOF'
cd / || exit 3
trap '' TERM
printf '%s 1 write\n' "$$" > "$KL_RUN/store-storage-request.tmp"
mv "$KL_RUN/store-storage-request.tmp" "$KL_RUN/store-storage-request"
exec sleep 1000
EOF
export KL_WATCH_EVERY=1
printf 'store\n' > "$STATE/codes"
sh "$SUP" &
sp=$!
for i in 1 2 3 4 5 6; do
    grep -q ' 1 paused$' "$KL_RUN/store-storage-reply" 2>/dev/null && break
    sleep 1
done
check "the Store held write access before shutdown" 'grep -q " 1 paused$" "$KL_RUN/store-storage-reply"'
read -r store_pid _ < "$KL_RUN/store-child.pid"
kill -9 "$sp"
for i in 1 2 3 4 5 6; do
    [ "$(cat "$STATE/fw")" = running ] && break
    sleep 1
done
check "Store exited before USB and stock UI restoration" '! kill -0 "$store_pid" 2>/dev/null && [ "$(cat "$STATE/fw")" = running ] && grep -q "State:.*S (sleeping)" "$KL_STORE_USB_PROC/42/status"'
check "watchdog cleared the Store lease" '[ ! -e "$KL_RUN/store-child.pid" ] && [ ! -e "$KL_RUN/store-storage-request" ]'
unset KL_WATCH_EVERY
teardown

echo "test: an opted-in native app runs from RAM and reserves storage when it saves"
setup
cat > "$ROOT/bin/usleep" <<'EOF'
#!/bin/sh
if [ "$1" -eq 100000 ]; then sleep 0.05; fi
EOF
cat > "$KL_DOCUMENTS/Native App.sh" <<'EOF'
#!/bin/sh
# PrimitiveUSBLease
[ "$KL_NATIVE_APP_RUN" = "$KL_RUN/native-app" ] && [ "$0" = "$KL_NATIVE_APP_RUN/launch" ] || exit 3
[ "$KL_STORE_USB_LEASE" = 0 ] && [ "$KL_STORE_OPEN_APP" = 0 ] || exit 3
cp "$STATE/native" "$KL_NATIVE_APP_RUN/run" && chmod 755 "$KL_NATIVE_APP_RUN/run" || exit 3
exec "$KL_NATIVE_APP_RUN/run"
EOF
cat > "$STATE/native" <<'EOF'
#!/bin/sh
[ "$(pwd)" = / ] && [ "$0" = "$KL_NATIVE_APP_RUN/run" ] && [ -p /dev/fd/1 ] || exit 3
echo 'native detached from flash' >> "$STATE/calls"
request() {
    printf '%s %s %s\n' "$$" "$1" "$2" > "$KL_RUN/store-storage-request.tmp"
    mv "$KL_RUN/store-storage-request.tmp" "$KL_RUN/store-storage-request"
}
until_matches() {
    for attempt in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
        grep -q "$1" "$2" 2>/dev/null && return 0
        sleep 0.1
    done
    exit 3
}
daemon=$KL_STORE_USB_PROC/42/status
mounts=$KL_STORE_USB_PROC/mounts
until_matches 'State:.*T (stopped)' "$daemon"
request 1 idle
until_matches 'State:.*S (sleeping)' "$daemon"
echo 'native idle permits USB' >> "$STATE/calls"
mv "$mounts" "$mounts.exported"
printf 'fsp /mnt/us fuse rw 0 0\n' > "$mounts"
request 2 write
until_matches "^$$ 2 unavailable$" "$KL_RUN/store-storage-reply"
echo 'native cannot write while the drive is exported' >> "$STATE/calls"
mv "$mounts.exported" "$mounts"
request 3 write
until_matches "^$$ 3 paused$" "$KL_RUN/store-storage-reply"
until_matches 'State:.*T (stopped)' "$daemon"
echo 'native saved locally' >> "$STATE/calls"
request 4 idle
until_matches 'State:.*S (sleeping)' "$daemon"
EOF
printf 'native\n0\n' > "$STATE/codes"
run_sup
check "native wrapper and executable have no working directory or log on flash" 'grep -q "native detached from flash" "$STATE/calls"'
check "the SDK can release USB while idle" 'grep -q "native idle permits USB" "$STATE/calls"'
check "exported storage rejects native writes" 'grep -q "native cannot write while the drive is exported" "$STATE/calls"'
check "native writes are allowed after eject under the same app PID" 'grep -q "native saved locally" "$STATE/calls"'
check "native RAM files and request are cleared when the app returns" '[ ! -e "$KL_RUN/native-app" ] && [ ! -e "$KL_RUN/store-storage-request" ] && [ "$(grep -c "launcher run:" "$STATE/calls")" -eq 2 ]'
teardown

echo "passed $PASS, failed $FAIL"
[ "$FAIL" -eq 0 ]
