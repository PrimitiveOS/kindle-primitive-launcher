#!/bin/sh
# The SSH server (port 2222) for Settings > SSH & DNS and the quick settings' SSH switch. It runs the Dropbear
# server and its zlib that the launcher installs in ssh/ (with the host key kept there too). The Kindle's firewall
# drops incoming connections, so start opens port 2222 and stop closes it again. The root password is set by `password`, which reads it from stdin: it is never put in a
# command line, logged, or kept in a file (only a flag records that it was set).
#
#   ssh.sh start | stop | status | check | password
set -u

HERE=$(cd "$(dirname "$0")" && pwd)
SSH_DIR="$HERE/ssh"
BIN="$SSH_DIR/bin"
LIB="$SSH_DIR/lib"
KEY="$SSH_DIR/dropbear_ed25519_host_key"
LOG="$SSH_DIR/dropbear.log"
PID=/tmp/dropbear_launcher.pid
TMP_SSH=/var/tmp/kl-ssh
IPT=$(command -v iptables || echo /usr/sbin/iptables)

running() { [ -f "$PID" ] && kill -0 "$(cat "$PID" 2>/dev/null)" 2>/dev/null; }

open_port() {
    $IPT -D INPUT -p tcp --dport 2222 -j ACCEPT 2>/dev/null
    $IPT -D OUTPUT -p tcp --sport 2222 -j ACCEPT 2>/dev/null
    $IPT -I INPUT -p tcp --dport 2222 -j ACCEPT && $IPT -I OUTPUT -p tcp --sport 2222 -j ACCEPT
}

close_port() {
    $IPT -D INPUT -p tcp --dport 2222 -j ACCEPT 2>/dev/null
    $IPT -D OUTPUT -p tcp --sport 2222 -j ACCEPT 2>/dev/null
}

case "$1" in
    start)
        if running; then open_port; echo "running"; exit 0; fi
        if grep -i ':08AE ' /proc/net/tcp 2>/dev/null | grep -q ' 0A '; then
            echo "port 2222 is already in use"
            exit 1
        fi
        [ -x "$BIN/dropbear" ] || { echo "no SSH server in $BIN: install the launcher again"; exit 1; }
        mkdir -p "$SSH_DIR"
        echo "started $(date)" >> "$LOG"
        # -R makes the host key if there is none yet; the key is kept, so the server keeps its identity.
        # The server runs from a copy in tmpfs, with / as its directory: nothing of it may hold /mnt/us, which the Kindle
        # unmounts to show its drive on a computer (and whatever still holds it is killed).
        mkdir -p "$TMP_SSH/lib" && cp "$BIN/dropbear" "$TMP_SSH/dropbear" || { echo "could not copy the server to $TMP_SSH"; exit 1; }
        # The helper loads zlib from the copy shipped beside it.
        if [ -f "$LIB/libz.so.1" ]; then
            cp "$LIB/libz.so.1" "$TMP_SSH/lib/" || { echo "could not copy the server library to $TMP_SSH"; exit 1; }
        fi
        chmod 755 "$TMP_SSH/dropbear"
        (cd / && LD_LIBRARY_PATH="$TMP_SSH/lib" "$TMP_SSH/dropbear" -E -R -r "$KEY" -p 2222 -P "$PID" 2>> "$TMP_SSH/dropbear.log")
        sleep 1
        if running; then open_port; echo "running"; else echo "failed"; exit 1; fi
        ;;
    stop)
        # The listener and the sessions it started: every process whose command line names dropbear (also a server that an
        # earlier build started), except this one.
        for p in /proc/[0-9]*; do
            pid=${p#/proc/}
            [ "$pid" = "$$" ] && continue
            case "$(tr '\0' ' ' 2>/dev/null < "$p/cmdline")" in *dropbear*) kill "$pid" 2>/dev/null ;; esac
        done
        rm -f "$PID"
        close_port
        echo "stopped"
        ;;
    check)
        if running; then echo "server running"; else echo "server not running"; fi
        if grep -i ':08AE ' /proc/net/tcp 2>/dev/null | grep -q ' 0A '; then echo "port 2222 listening"; else echo "port 2222 not listening"; fi
        if $IPT -L INPUT -n 2>/dev/null | grep -q 'dpt:2222'; then echo "firewall open"; else echo "firewall closed"; fi
        ;;
    password)
        IFS= read -r pw || pw=""
        case "$pw" in
            ""|*[!A-Za-z0-9._@#%+=!-]*) echo "use 6 to 64 letters, digits or - _ . @ # % + = !, no spaces"; exit 1 ;;
        esac
        n=${#pw}
        [ "$n" -ge 6 ] && [ "$n" -le 64 ] || { echo "use 6 to 64 characters"; exit 1; }
        mount -o remount,rw / || { echo "could not make the system partition writable"; exit 1; }
        printf 'root:%s\n' "$pw" | chpasswd
        r=$?
        sync
        mount -o remount,ro / 2>/dev/null || echo "the system partition stays writable until restart"
        if [ $r -eq 0 ]; then
            mkdir -p "$SSH_DIR" && : > "$SSH_DIR/password-set"
            echo "password set"
            exit 0
        fi
        echo "could not set the password"
        exit 1
        ;;
    *)
        if running; then echo "running"; else echo "stopped"; fi
        ;;
esac
