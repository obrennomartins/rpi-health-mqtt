#!/bin/sh
set -eu
set -f

PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
LC_ALL=C
export PATH LC_ALL

PROJECT_NAME=rpi-health-mqtt
SERVICE_NAME=rpi-health-mqtt.service
SERVICE_USER=rpi-health-mqtt
SERVICE_GROUP=rpi-health-mqtt

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
Usage: scripts/install.sh [--binary PATH]

Install rpi-health-mqtt and its systemd service. By default, the installer
uses target/armv7-unknown-linux-gnueabihf/release/rpi-health-mqtt.

Environment variables:
  DESTDIR      Existing absolute directory that prefixes every installed path.
               Staging never creates accounts or invokes systemctl. The
               filesystem root is not a valid staging directory. The supplied
               binary is still checked with file and readelf.
  SYSTEMCTL    Alternate systemctl executable for a live installation only.
               Commands are executed directly, never evaluated by a shell.
EOF
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

path_exists() {
    [ -e "$1" ] || [ -L "$1" ]
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

create_directory_chain() {
    create_target=$1
    create_description=$2
    validate_directory_chain "$create_target" "$create_description"

    if [ -n "$DESTDIR" ]; then
        create_current=$DESTDIR
        create_relative=${create_target#"$DESTDIR"/}
    else
        create_current=
        create_relative=${create_target#/}
    fi

    create_saved_ifs=$IFS
    IFS=/
    # Splitting the fixed managed suffix into path components is intentional.
    # shellcheck disable=SC2086
    set -- $create_relative
    IFS=$create_saved_ifs

    for create_component do
        create_current=$create_current/$create_component
        reject_symlink "$create_current" "$create_description component"
        if [ -e "$create_current" ]; then
            [ -d "$create_current" ] ||
                die "$create_description component is not a directory: $create_current"
        else
            install -d -o root -g root -m 0755 -- "$create_current"
        fi
        reject_symlink "$create_current" "$create_description component"
        [ -d "$create_current" ] ||
            die "$create_description component is not a directory: $create_current"
        assert_resolved_under_stage "$create_current"
    done
}

ensure_public_directory() {
    create_directory_chain "$1" "installation directory"
}

ensure_private_directory() {
    private_path=$1
    private_owner_group=$2
    create_directory_chain "$private_path" "configuration directory"
    chown "root:$private_owner_group" -- "$private_path"
    chmod 0750 -- "$private_path"
    reject_symlink "$private_path" "configuration directory"
    assert_resolved_under_stage "$private_path"
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

secure_existing_file() {
    secure_path=$1
    secure_owner_group=$2
    secure_description=$3

    validate_managed_file "$secure_path" "$secure_description"
    [ -e "$secure_path" ] ||
        die "$secure_description disappeared during installation: $secure_path"
    chown "root:$secure_owner_group" -- "$secure_path"
    chmod 0640 -- "$secure_path"
    validate_managed_file "$secure_path" "$secure_description"
}

install_managed_file() {
    managed_source=$1
    managed_destination=$2
    managed_owner_group=$3
    managed_mode=$4
    managed_install_description=$5

    validate_managed_file "$managed_destination" "$managed_install_description"
    install -o root -g "$managed_owner_group" -m "$managed_mode" \
        -- "$managed_source" "$managed_destination"
    validate_managed_file "$managed_destination" "$managed_install_description"
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

account_entry() {
    database=$1
    account_name=$2
    entry=$(getent "$database" "$account_name" 2>/dev/null) || return 1
    line_count=$(printf '%s\n' "$entry" | awk 'END { print NR }')
    [ "$line_count" = 1 ] ||
        die "account lookup returned multiple entries for $account_name"
    entry_name=$(printf '%s\n' "$entry" | awk -F: 'NR == 1 { print $1 }')
    [ "$entry_name" = "$account_name" ] ||
        die "account lookup returned an unexpected name for $account_name"
    ACCOUNT_ENTRY=$entry
    return 0
}

account_field() {
    field_number=$1
    printf '%s\n' "$ACCOUNT_ENTRY" |
        awk -F: -v field_number="$field_number" 'NR == 1 { print $field_number }'
}

system_uid_max() {
    maximum=$(awk '
        $1 == "SYS_UID_MAX" && $2 ~ /^[0-9]+$/ {
            print $2
            exit
        }
    ' /etc/login.defs 2>/dev/null || true)
    if [ -n "$maximum" ]; then
        printf '%s\n' "$maximum"
    else
        printf '%s\n' 999
    fi
}

system_gid_max() {
    maximum=$(awk '
        $1 == "SYS_GID_MAX" && $2 ~ /^[0-9]+$/ {
            print $2
            exit
        }
    ' /etc/login.defs 2>/dev/null || true)
    if [ -n "$maximum" ]; then
        printf '%s\n' "$maximum"
    else
        printf '%s\n' 999
    fi
}

validate_service_group() {
    account_entry group "$SERVICE_GROUP" ||
        die "service group does not exist: $SERVICE_GROUP"
    group_gid=$(account_field 3)
    group_members=$(account_field 4)
    case "$group_gid" in
        ''|*[!0-9]*)
            die "service group has an invalid numeric ID"
            ;;
    esac
    maximum_gid=$(system_gid_max)
    if [ "$group_gid" -le 0 ] || [ "$group_gid" -gt "$maximum_gid" ]; then
        die "service group is not an unprivileged system group"
    fi
    [ -z "$group_members" ] ||
        die "service group has unexpected explicit members"
    group_id_matches=$(
        getent group |
            awk -F: -v expected_gid="$group_gid" '$3 == expected_gid { print $1 }'
    )
    [ "$group_id_matches" = "$SERVICE_GROUP" ] ||
        die "service group ID is shared with another group"
    SERVICE_GROUP_GID=$group_gid
}

validate_service_user() {
    account_entry passwd "$SERVICE_USER" ||
        die "service user does not exist: $SERVICE_USER"
    user_uid=$(account_field 3)
    user_gid=$(account_field 4)
    user_home=$(account_field 6)
    user_shell=$(account_field 7)

    case "$user_uid" in
        ''|*[!0-9]*)
            die "service user has an invalid numeric ID"
            ;;
    esac
    maximum_uid=$(system_uid_max)
    if [ "$user_uid" -le 0 ] || [ "$user_uid" -gt "$maximum_uid" ]; then
        die "service user is not an unprivileged system account"
    fi
    user_id_matches=$(
        getent passwd |
            awk -F: -v expected_uid="$user_uid" '$3 == expected_uid { print $1 }'
    )
    [ "$user_id_matches" = "$SERVICE_USER" ] ||
        die "service user ID is shared with another account"
    [ "$user_gid" = "$SERVICE_GROUP_GID" ] ||
        die "service user has an unexpected primary group"
    [ "$user_home" = /nonexistent ] ||
        die "service user has an unexpected home directory"
    case "$user_shell" in
        /usr/sbin/nologin|/sbin/nologin|/bin/false)
            [ -x "$user_shell" ] ||
                die "service user's non-login shell is not executable"
            ;;
        *)
            die "service user has an interactive or unexpected shell"
            ;;
    esac

    group_names=$(id -nG "$SERVICE_USER") ||
        die "service user's supplementary groups could not be read"
    saw_service_group=false
    saw_video_group=false
    # NSS group names cannot contain spaces.
    # shellcheck disable=SC2086
    for group_name in $group_names; do
        case "$group_name" in
            "$SERVICE_GROUP")
                saw_service_group=true
                ;;
            video)
                saw_video_group=true
                ;;
            *)
                die "service user has an unexpected supplementary group"
                ;;
        esac
    done
    [ "$saw_service_group" = true ] ||
        die "service user is missing its primary service group"
    [ "$saw_video_group" = true ] ||
        die "service user is missing the video group"
}

find_nologin_shell() {
    for candidate in /usr/sbin/nologin /sbin/nologin /bin/false; do
        if [ -x "$candidate" ]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done
    return 1
}

ensure_service_account() {
    require_command getent
    require_command groupadd
    require_command useradd
    require_command id
    require_command awk

    group_exists=false
    user_exists=false
    if account_entry group "$SERVICE_GROUP"; then
        group_exists=true
        validate_service_group
    fi
    if account_entry passwd "$SERVICE_USER"; then
        user_exists=true
    fi

    if [ "$user_exists" = true ] && [ "$group_exists" = false ]; then
        die "service user exists without the dedicated service group"
    fi

    if [ "$user_exists" = true ]; then
        account_entry group video ||
            die "video group is missing for the existing service user"
        validate_service_user
        return 0
    fi

    if [ "$group_exists" = false ]; then
        groupadd --system "$SERVICE_GROUP"
    fi
    if ! account_entry group video; then
        warn "the video group does not exist; creating it as a system group"
        groupadd --system video
    fi

    nologin_shell=$(find_nologin_shell) ||
        die "no non-login shell was found"
    useradd \
        --system \
        --gid "$SERVICE_GROUP" \
        --groups video \
        --home-dir /nonexistent \
        --no-create-home \
        --shell "$nologin_shell" \
        "$SERVICE_USER"

    validate_service_group
    validate_service_user
}

validate_armv7_binary() {
    require_command file
    require_command readelf
    require_command grep

    file_description=$(file -b -- "$SOURCE_BINARY" 2>/dev/null) ||
        die "binary type could not be identified"
    printf '%s\n' "$file_description" |
        grep -Eq 'ELF 32-bit LSB.*ARM.*EABI5' ||
        die "binary is not an ARM EABI5 ELF32 executable"

    elf_header=$(readelf -h -- "$SOURCE_BINARY" 2>/dev/null) ||
        die "binary is not a readable ELF executable"
    printf '%s\n' "$elf_header" | grep -Eq 'Class:[[:space:]]+ELF32' ||
        die "binary is not an ELF32 executable"
    printf '%s\n' "$elf_header" | grep -Eq 'Data:.*little endian' ||
        die "binary is not little-endian"
    printf '%s\n' "$elf_header" | grep -Eq 'Machine:[[:space:]]+ARM' ||
        die "binary does not target ARM"
    printf '%s\n' "$elf_header" | grep -Eq 'Flags:.*hard-float ABI' ||
        die "binary does not use the ARM hard-float ABI"

    elf_attributes=$(readelf -A -- "$SOURCE_BINARY" 2>/dev/null) ||
        die "binary ARM attributes could not be read"
    printf '%s\n' "$elf_attributes" |
        grep -Eq 'Tag_CPU_arch:[[:space:]]+v7([^0-9]|$)' ||
        die "binary does not target the ARMv7 architecture"
    printf '%s\n' "$elf_attributes" |
        grep -Eq 'Tag_ABI_VFP_args:[[:space:]]+VFP registers' ||
        die "binary does not declare VFP register arguments"

    log "Validated ARMv7 hard-float binary."
}

validate_live_platform() {
    require_command uname
    require_command dpkg

    kernel_architecture=$(uname -m)
    case "$kernel_architecture" in
        armv7|armv7l|armv7-*)
            log "Detected supported ARMv7 kernel architecture: $kernel_architecture"
            ;;
        *)
            die "unsupported kernel architecture: $kernel_architecture (expected ARMv7)"
            ;;
    esac

    userland_architecture=$(dpkg --print-architecture 2>/dev/null) ||
        die "Debian userland architecture could not be detected"
    [ "$userland_architecture" = armhf ] ||
        die "unsupported userland architecture: $userland_architecture (expected armhf)"
    log "Detected supported armhf userland."
}

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
PROJECT_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd -P)
DEFAULT_BINARY="$PROJECT_ROOT/target/armv7-unknown-linux-gnueabihf/release/$PROJECT_NAME"
SOURCE_BINARY=$DEFAULT_BINARY

while [ "$#" -gt 0 ]; do
    case "$1" in
        -b|--binary)
            [ "$#" -ge 2 ] || die "$1 requires a path"
            SOURCE_BINARY=$2
            shift 2
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

[ "$(id -u)" -eq 0 ] || die "this installer must run as root"

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

BIN_DIR="$DESTDIR/usr/local/bin"
CONFIG_DIR="$DESTDIR/etc/$PROJECT_NAME"
UNIT_DIR="$DESTDIR/etc/systemd/system"
DESTINATION_BINARY="$BIN_DIR/$PROJECT_NAME"
CONFIG_FILE="$CONFIG_DIR/config.toml"
PASSWORD_FILE="$CONFIG_DIR/mqtt-password"
DESTINATION_UNIT="$UNIT_DIR/$SERVICE_NAME"
SOURCE_CONFIG="$PROJECT_ROOT/config/config.example.toml"
SOURCE_UNIT="$PROJECT_ROOT/systemd/$SERVICE_NAME"

require_command install
require_command chown
require_command chmod
require_command stat
require_command dirname
if [ -n "$DESTDIR" ]; then
    require_command readlink
fi

if [ ! -f "$SOURCE_BINARY" ] || [ ! -r "$SOURCE_BINARY" ]; then
    die "binary is not a readable regular file: $SOURCE_BINARY"
fi
if [ ! -f "$SOURCE_CONFIG" ] || [ ! -r "$SOURCE_CONFIG" ]; then
    die "example configuration not found: $SOURCE_CONFIG"
fi
if [ ! -f "$SOURCE_UNIT" ] || [ ! -r "$SOURCE_UNIT" ]; then
    die "systemd unit not found: $SOURCE_UNIT"
fi

validate_directory_chain "$BIN_DIR" "binary installation directory"
validate_directory_chain "$CONFIG_DIR" "configuration directory"
validate_directory_chain "$UNIT_DIR" "systemd unit directory"
validate_managed_file "$DESTINATION_BINARY" "installed binary"
validate_managed_file "$DESTINATION_UNIT" "installed systemd unit"
validate_managed_file "$CONFIG_FILE" "configuration file"
validate_managed_file "$PASSWORD_FILE" "password file"
validate_armv7_binary

if [ -n "$DESTDIR" ]; then
    FILE_GROUP=root
    log "Staging mode: platform, account, and systemd operations are skipped"
else
    SYSTEMCTL=${SYSTEMCTL-systemctl}
    [ -n "$SYSTEMCTL" ] || die "SYSTEMCTL must not be empty"
    resolve_systemctl
    validate_live_platform
    ensure_service_account
    FILE_GROUP=$SERVICE_GROUP
fi

ensure_public_directory "$BIN_DIR"
ensure_public_directory "$UNIT_DIR"
ensure_private_directory "$CONFIG_DIR" "$FILE_GROUP"

install_managed_file "$SOURCE_BINARY" "$DESTINATION_BINARY" root 0755 "installed binary"
install_managed_file "$SOURCE_UNIT" "$DESTINATION_UNIT" root 0644 "installed systemd unit"

if ! path_exists "$CONFIG_FILE"; then
    install_managed_file "$SOURCE_CONFIG" "$CONFIG_FILE" "$FILE_GROUP" 0640 "configuration file"
    log "Installed example configuration: $CONFIG_FILE"
else
    secure_existing_file "$CONFIG_FILE" "$FILE_GROUP" "configuration file"
    log "Preserved existing configuration: $CONFIG_FILE"
fi

if path_exists "$PASSWORD_FILE"; then
    secure_existing_file "$PASSWORD_FILE" "$FILE_GROUP" "password file"
    log "Preserved existing MQTT password file"
fi

if [ -z "$DESTDIR" ]; then
    "$SYSTEMCTL" daemon-reload
    if [ -s "$CONFIG_FILE" ] && [ -s "$PASSWORD_FILE" ]; then
        "$SYSTEMCTL" enable "$SERVICE_NAME"
        "$SYSTEMCTL" restart "$SERVICE_NAME"
        log "Enabled and started $SERVICE_NAME"
    else
        warn "service was not enabled or started because configuration or password is missing or empty"
    fi
else
    log "Skipped systemd commands in staging mode"
fi

log "Installation completed."
if [ -z "$DESTDIR" ]; then
    log "Verify configuration: $DESTINATION_BINARY check --config $CONFIG_FILE"
    log "Inspect service status: systemctl status $SERVICE_NAME"
    log "Follow service logs: journalctl -u $SERVICE_NAME -f"
else
    log "Staged root: $DESTDIR"
fi
