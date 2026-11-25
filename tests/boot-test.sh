#!/bin/sh
# Runs deploy/boot.sh on the Mac with a stub supervisor. Each "reboot" removes the once-per-boot marker.
# Checks: normal boot, the healthy reset, the third failed boot disables autostart, the escape hatches,
# and that a second start in the same boot does nothing.
#
#   sh tests/boot-test.sh
set -u

HERE=$(cd "$(dirname "$0")/.." && pwd)
BOOT="$HERE/deploy/boot.sh"
PASS=0
FAIL=0

setup() {
    ROOT=$(mktemp -d /tmp/kl-boot-test.XXXXXX)
    export KL_DIR="$ROOT/dir" KL_RUN="$ROOT/run" KL_BOOT_ONCE="$ROOT/once" KL_BOOT_KINDLE="$ROOT/BOOT_KINDLE"
    export KL_SETTLE_SECS=0 KL_HEALTHY_SECS=60 STATE="$ROOT/state"
    mkdir -p "$KL_DIR" "$STATE"
    : > "$STATE/calls"
    # The stub supervisor: "quick" exits at once (a launcher that cannot stay up); "healthy" runs for 3 s.
    cat > "$KL_DIR/supervisor.sh" <<'SUP'
#!/bin/sh
echo "supervisor ran" >> "$STATE/calls"
if [ "$(cat "$STATE/sup-mode" 2>/dev/null)" = healthy ]; then sleep 3; fi
exit 1
SUP
    echo quick > "$STATE/sup-mode"
}

teardown() { rm -rf "$ROOT"; }

reboot() { rm -f "$KL_BOOT_ONCE"; }

boot() { sh "$BOOT"; }

count_runs() { grep -c "supervisor ran" "$STATE/calls"; }

check() {
    if eval "$2"; then
        PASS=$((PASS + 1))
    else
        FAIL=$((FAIL + 1))
        echo "FAIL: $1"
        echo "--- calls"; cat "$STATE/calls"; echo "--- log"; cat "$KL_DIR/log-launcher.txt" 2>/dev/null
    fi
}

echo "test: a launcher that cannot stay up disables autostart on the third boot"
setup
boot; check "boot 1 started the launcher" '[ "$(count_runs)" = 1 ]'
check "boot 1 counted" '[ "$(cat "$KL_DIR/boot-attempts")" = 1 ]'
reboot; boot; check "boot 2 started the launcher" '[ "$(count_runs)" = 2 ]'
reboot; boot
check "boot 3 did not start the launcher" '[ "$(count_runs)" = 2 ]'
check "DISABLE created" '[ -e "$KL_DIR/DISABLE" ]'
check "count reset, so re-enabling starts from zero" '[ "$(cat "$KL_DIR/boot-attempts")" = 0 ]'
reboot; boot
check "DISABLE keeps the launcher off" '[ "$(count_runs)" = 2 ]'
rm -f "$KL_DIR/DISABLE"; reboot; boot
check "after DISABLE is removed the next boot starts the launcher" '[ "$(count_runs)" = 3 ]'
teardown

echo "test: a healthy launcher resets the boot count"
setup
echo healthy > "$STATE/sup-mode"
export KL_HEALTHY_SECS=1
boot
export KL_HEALTHY_SECS=60
check "the count was reset after the healthy period" '[ "$(cat "$KL_DIR/boot-attempts")" = 0 ]'
check "a reset was logged" 'grep -q "boot count reset" "$KL_DIR/log-launcher.txt"'
teardown

echo "test: BOOT_KINDLE gives the stock UI this boot only"
setup
: > "$KL_BOOT_KINDLE"
boot
check "no launcher" '[ "$(count_runs)" = 0 ]'
check "no count" '[ ! -f "$KL_DIR/boot-attempts" ]'
rm -f "$KL_BOOT_KINDLE"; reboot; boot
check "the next boot starts the launcher" '[ "$(count_runs)" = 1 ]'
teardown

echo "test: the guard runs once per boot"
setup
boot; boot
check "a second start in the same boot does nothing" '[ "$(count_runs)" = 1 ]'
teardown

echo "test: a missing supervisor gives the stock UI"
setup
rm -f "$KL_DIR/supervisor.sh"
boot
check "logged" 'grep -q "no supervisor.sh" "$KL_DIR/log-launcher.txt"'
teardown

echo "test: an unfinished update starts with the saved supervisor"
setup
mv "$KL_DIR/supervisor.sh" "$KL_DIR/supervisor.sh.prev"
mkdir -p "$KL_DIR/store"
: > "$KL_DIR/store/journal.json"
: > "$KL_DIR/kl-launcher.prev"
echo 2 > "$KL_DIR/boot-attempts"
printf '#!/bin/sh\necho broken >> "$STATE/calls"\n' > "$KL_DIR/supervisor.sh"
boot
check "saved supervisor ran" '[ "$(count_runs)" = 1 ]'
check "candidate supervisor left alone" '! grep -q broken "$STATE/calls"'
check "generic boot limit did not prevent trial recovery" '[ ! -f "$KL_DIR/DISABLE" ]'
teardown

echo "passed $PASS, failed $FAIL"
[ "$FAIL" -eq 0 ]
