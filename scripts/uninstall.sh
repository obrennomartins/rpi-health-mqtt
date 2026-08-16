#!/bin/sh
set -eu
set -f

PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
LC_ALL=C
export PATH LC_ALL

PROJECT_NAME=rpi-health-mqtt
SERVICE_NAME=rpi-health-mqtt.service

log() {
    printf '%s\n' "$*"
}

warn() {
    printf 'Warning: %s\n' "$*" >&2
}

die() {
    printf 'Error: %s\n' "$*" >&2
    exit 1
}

usage() {
    cat <<'EOF'
Usage: scripts/uninstall.sh [--purge-config]

Remove the executable and systemd unit. Configuration, the MQTT password,
and the service account are preserved by default.

Options:
  --purge-config  Remove the managed configuration and password files. Any
                  unrecognized files in the configuration directory remain.

Environment variables:
  DESTDIR      Existing absolute directory that prefixes every managed path.
               Staging never invokes systemctl. The filesystem root is not a
               valid staging directory.
  SYSTEMCTL    Alternate systemctl executable for a live uninstallation only.
               Commands are executed directly, never evaluated by a shell.
EOF
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

reject_symlink() {
    reject_path=$1
    reject_description=$2
    [ ! -L "$reject_path" ] ||
        die "$reject_description must not be a symbolic link: $reject_path"
}

assert_stage_descendant() {
    descendant_path=$1
    [ -n "$DESTDIR" ] || return 0
    case "$descendant_path" in
        "$DESTDIR"/*)
            ;;
        *)
            die "managed path is outside the canonical staging root: $descendant_path"
            ;;
    esac
}

validate_directory_chain() {
    chain_target=$1
    chain_description=$2

    case "$chain_target" in
        /*)
            ;;
        *)
            die "$chain_description must be an absolute path: $chain_target"
            ;;
    esac

    if [ -n "$DESTDIR" ]; then
        assert_stage_descendant "$chain_target"
        chain_current=$DESTDIR
        chain_relative=${chain_target#"$DESTDIR"/}
    else
        chain_current=
        chain_relative=${chain_target#/}
    fi

    chain_saved_ifs=$IFS
    IFS=/
    # Splitting the fixed managed suffix into path components is intentional.
    # shellcheck disable=SC2086
    set -- $chain_relative
    IFS=$chain_saved_ifs

    for chain_component do
        [ -n "$chain_component" ] ||
            die "invalid empty path component in $chain_target"
        chain_current=$chain_current/$chain_component
        reject_symlink "$chain_current" "$chain_description component"
        if [ -e "$chain_current" ] && [ ! -d "$chain_current" ]; then
            die "$chain_description component is not a directory: $chain_current"
        fi
    done
}

assert_resolved_under_stage() {
    resolved_path=$1
    [ -n "$DESTDIR" ] || return 0
    resolved_value=$(readlink -f -- "$resolved_path") ||
        die "managed path could not be resolved: $resolved_path"
    case "$resolved_value" in
        "$DESTDIR"/*)
            ;;
        *)
            die "managed path resolved outside the canonical staging root: $resolved_path"
            ;;
    esac
}

validate_managed_file() {
    managed_path=$1
    managed_description=$2
    managed_parent=$(dirname -- "$managed_path")

    validate_directory_chain "$managed_parent" "$managed_description parent directory"
    assert_stage_descendant "$managed_path"
    reject_symlink "$managed_path" "$managed_description"
    if [ -e "$managed_path" ]; then
        [ -f "$managed_path" ] ||
            die "$managed_description is not a regular file: $managed_path"
        managed_link_count=$(stat -c '%h' -- "$managed_path") ||
            die "$managed_description link count could not be read: $managed_path"
        [ "$managed_link_count" = 1 ] ||
            die "$managed_description must have exactly one hard link: $managed_path"
        assert_resolved_under_stage "$managed_path"
    fi
}

remove_managed_file() {
    removal_path=$1
    removal_description=$2

    validate_managed_file "$removal_path" "$removal_description"
    if [ -e "$removal_path" ]; then
        rm -- "$removal_path"
    fi
    if [ -e "$removal_path" ] || [ -L "$removal_path" ]; then
        die "$removal_description was not removed: $removal_path"
    fi
}

resolve_systemctl() {
    case "$SYSTEMCTL" in
        */*)
            [ -x "$SYSTEMCTL" ] || die "SYSTEMCTL is not executable: $SYSTEMCTL"
            ;;
        *)
            command -v "$SYSTEMCTL" >/dev/null 2>&1 ||
                die "systemctl executable not found: $SYSTEMCTL"
            ;;
    esac
}

stop_and_disable_service() {
    if "$SYSTEMCTL" disable --now "$SERVICE_NAME"; then
        return 0
    fi

    load_state=$(
        "$SYSTEMCTL" show "$SERVICE_NAME" --property=LoadState --value
    ) || die "systemd could not verify the unit load state after stop failure"
    active_state=$(
        "$SYSTEMCTL" show "$SERVICE_NAME" --property=ActiveState --value
    ) || die "systemd could not verify the unit active state after stop failure"

    [ "$active_state" = inactive ] ||
        die "systemd did not verify an inactive service; managed files were preserved"
    if [ "$load_state" = not-found ]; then
        warn "the unit was already absent after systemd could not disable it"
    else
        warn "the unit is verified inactive but could not be disabled"
    fi
}

PURGE_CONFIG=false
while [ "$#" -gt 0 ]; do
    case "$1" in
        --purge-config)
            PURGE_CONFIG=true
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        --)
            shift
            [ "$#" -eq 0 ] || die "unexpected positional argument: $1"
            ;;
        *)
            die "unknown argument: $1"
            ;;
    esac
done

[ "$(id -u)" -eq 0 ] || die "this uninstaller must run as root"

raw_destdir=${DESTDIR-}
if [ -n "$raw_destdir" ]; then
    case "$raw_destdir" in
        /*)
            ;;
        *)
            die "DESTDIR must be an absolute path"
            ;;
    esac
    [ -d "$raw_destdir" ] ||
        die "DESTDIR must name an existing directory"
    DESTDIR=$(CDPATH='' cd -- "$raw_destdir" && pwd -P) ||
        die "DESTDIR could not be resolved"
    [ "$DESTDIR" != / ] ||
        die "DESTDIR must not resolve to the filesystem root"
else
    DESTDIR=
fi

DESTINATION_BINARY="$DESTDIR/usr/local/bin/$PROJECT_NAME"
CONFIG_DIR="$DESTDIR/etc/$PROJECT_NAME"
CONFIG_FILE="$CONFIG_DIR/config.toml"
PASSWORD_FILE="$CONFIG_DIR/mqtt-password"
DESTINATION_UNIT="$DESTDIR/etc/systemd/system/$SERVICE_NAME"

require_command dirname
require_command stat
require_command rm
if [ -n "$DESTDIR" ]; then
    require_command readlink
fi

validate_managed_file "$DESTINATION_BINARY" "installed binary"
validate_managed_file "$DESTINATION_UNIT" "installed systemd unit"
validate_directory_chain "$CONFIG_DIR" "configuration directory"
validate_managed_file "$CONFIG_FILE" "configuration file"
validate_managed_file "$PASSWORD_FILE" "password file"

if [ -z "$DESTDIR" ]; then
    SYSTEMCTL=${SYSTEMCTL-systemctl}
    [ -n "$SYSTEMCTL" ] || die "SYSTEMCTL must not be empty"
    resolve_systemctl
    stop_and_disable_service
fi

remove_managed_file "$DESTINATION_BINARY" "installed binary"
remove_managed_file "$DESTINATION_UNIT" "installed systemd unit"

if [ -z "$DESTDIR" ]; then
    "$SYSTEMCTL" daemon-reload
else
    log "Skipped systemd commands in staging mode"
fi

if [ "$PURGE_CONFIG" = true ]; then
    remove_managed_file "$CONFIG_FILE" "configuration file"
    remove_managed_file "$PASSWORD_FILE" "password file"
    if [ -d "$CONFIG_DIR" ]; then
        if rmdir -- "$CONFIG_DIR" 2>/dev/null; then
            log "Removed empty configuration directory: $CONFIG_DIR"
        else
            warn "configuration directory contains unrecognized files and was preserved: $CONFIG_DIR"
        fi
    fi
else
    log "Preserved configuration and MQTT password under: $CONFIG_DIR"
fi

log "Preserved service account: rpi-health-mqtt"
log "Uninstallation completed."
