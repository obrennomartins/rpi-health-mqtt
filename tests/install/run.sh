#!/bin/sh
set -eu
set -f

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

assert_file() {
    if [ ! -f "$1" ] || [ -L "$1" ]; then
        fail "expected regular file: $1"
    fi
}

assert_absent() {
    if [ -e "$1" ] || [ -L "$1" ]; then
        fail "expected path to be absent: $1"
    fi
}

assert_mode() {
    actual_mode=$(stat -c '%a' -- "$1")
    [ "$actual_mode" = "$2" ] ||
        fail "expected mode $2 for $1, got $actual_mode"
}

assert_owner() {
    actual_owner=$(stat -c '%U:%G' -- "$1")
    [ "$actual_owner" = "$2" ] ||
        fail "expected owner $2 for $1, got $actual_owner"
}

assert_log_line() {
    grep -F -x -- "$2" "$1" >/dev/null 2>&1 ||
        fail "missing systemctl invocation: $2"
}

assert_secret_absent() {
    secret=$1
    shift
    for inspected_file do
        if grep -F -- "$secret" "$inspected_file" >/dev/null 2>&1; then
            fail "secret canary was disclosed in $inspected_file"
        fi
    done
}

expect_failure() {
    description=$1
    output_file=$2
    shift 2
    if "$@" >"$output_file" 2>&1; then
        fail "expected failure: $description"
    fi
}

account_cleanup() {
    if getent passwd rpi-health-mqtt >/dev/null 2>&1; then
        userdel rpi-health-mqtt >/dev/null 2>&1
    fi
    if getent group rpi-health-mqtt >/dev/null 2>&1; then
        groupdel -f rpi-health-mqtt >/dev/null 2>&1
    fi
}

[ "$(id -u)" -eq 0 ] ||
    fail "installer tests must run as root inside an isolated container"
[ "${INSTALL_TEST_CONTAINER:-}" = 1 ] ||
    fail "INSTALL_TEST_CONTAINER=1 is required"
[ -f /run/rpi-health-mqtt-test-container ] ||
    fail "installer tests refuse to modify live paths outside a Docker container"

TEST_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
PROJECT_ROOT=$(CDPATH='' cd -- "$TEST_DIR/../.." && pwd -P)
INSTALL_SCRIPT="$PROJECT_ROOT/scripts/install.sh"
UNINSTALL_SCRIPT="$PROJECT_ROOT/scripts/uninstall.sh"
: "${INSTALL_TEST_ARM_BINARY:?INSTALL_TEST_ARM_BINARY must name the cross-built release binary}"
staging_binary=$(readlink -f -- "$INSTALL_TEST_ARM_BINARY") ||
    fail "cross-built release binary could not be resolved"
assert_file "$staging_binary"

scratch=$(mktemp -d)
test_phase=initialization
video_group_created=false
fake_uname_path=/usr/local/sbin/uname
fake_dpkg_path=/usr/local/sbin/dpkg
live_binary=/usr/local/bin/rpi-health-mqtt
live_unit=/etc/systemd/system/rpi-health-mqtt.service
live_config_dir=/etc/rpi-health-mqtt

cleanup() {
    cleanup_status=$?
    trap - EXIT HUP INT TERM
    set +e
    rm -f -- "$fake_uname_path" "$fake_dpkg_path"
    rm -f -- "$live_binary" "$live_unit"
    rm -rf -- "$live_config_dir"
    account_cleanup
    if [ "$video_group_created" = true ]; then
        groupdel video
    fi
    rm -rf -- "$scratch"
    if [ "$cleanup_status" -ne 0 ]; then
        printf 'FAIL: installer test phase failed: %s\n' "$test_phase" >&2
    fi
    exit "$cleanup_status"
}
trap cleanup EXIT HUP INT TERM

assert_absent "$fake_uname_path"
assert_absent "$fake_dpkg_path"
assert_absent "$live_binary"
assert_absent "$live_unit"
assert_absent "$live_config_dir"
if getent passwd rpi-health-mqtt >/dev/null 2>&1 ||
    getent group rpi-health-mqtt >/dev/null 2>&1; then
    fail "isolated container unexpectedly contains the service account"
fi

fake_systemctl="$scratch/systemctl tool/systemctl"
systemctl_log="$scratch/systemctl.log"
mkdir -p -- "$(dirname -- "$fake_systemctl")"
install -m 0755 -- "$TEST_DIR/fake-systemctl.sh" "$fake_systemctl"
: >"$systemctl_log"

stage="$scratch/staged root"
mkdir -p -- "$stage"
stage_output="$scratch/stage-install.out"
test_phase=initial-staging-install
DESTDIR="$stage" \
SYSTEMCTL="$fake_systemctl" \
SYSTEMCTL_LOG="$systemctl_log" \
SYSTEMCTL_FAIL_IF_CALLED=true \
    sh "$INSTALL_SCRIPT" --binary "$staging_binary" >"$stage_output" 2>&1

installed_binary="$stage/usr/local/bin/rpi-health-mqtt"
installed_unit="$stage/etc/systemd/system/rpi-health-mqtt.service"
config_dir="$stage/etc/rpi-health-mqtt"
config_file="$config_dir/config.toml"
password_file="$config_dir/mqtt-password"

assert_file "$installed_binary"
assert_file "$installed_unit"
assert_file "$config_file"
assert_absent "$password_file"
assert_mode "$installed_binary" 755
assert_mode "$installed_unit" 644
assert_mode "$config_dir" 750
assert_mode "$config_file" 640
assert_owner "$installed_binary" root:root
assert_owner "$config_file" root:root
cmp -s -- "$PROJECT_ROOT/systemd/rpi-health-mqtt.service" "$installed_unit" ||
    fail "installed systemd unit differs from the repository unit"
cmp -s -- "$PROJECT_ROOT/config/config.example.toml" "$config_file" ||
    fail "new installation did not use the example configuration"
[ ! -s "$systemctl_log" ] ||
    fail "staging invoked systemctl even though SYSTEMCTL was explicitly set"

printf '# preserved local configuration\n' >"$config_file"
secret_canary='install-test-secret-canary-49173'
printf '%s\n' "$secret_canary" >"$password_file"
chmod 0777 -- "$config_file" "$password_file"
config_checksum=$(cksum <"$config_file")
password_checksum=$(cksum <"$password_file")

: >"$stage_output"
test_phase=idempotent-staging-install
DESTDIR="$stage" \
SYSTEMCTL="$fake_systemctl" \
SYSTEMCTL_LOG="$systemctl_log" \
SYSTEMCTL_FAIL_IF_CALLED=true \
    sh "$INSTALL_SCRIPT" --binary "$staging_binary" >"$stage_output" 2>&1
[ "$(cksum <"$config_file")" = "$config_checksum" ] ||
    fail "installer overwrote the existing configuration"
[ "$(cksum <"$password_file")" = "$password_checksum" ] ||
    fail "installer overwrote the existing password"
assert_mode "$config_file" 640
assert_mode "$password_file" 640
assert_owner "$password_file" root:root
assert_secret_absent "$secret_canary" "$stage_output" "$systemctl_log"
[ ! -s "$systemctl_log" ] || fail "repeated staging invoked systemctl"

test_phase=default-staging-uninstall
DESTDIR="$stage" \
SYSTEMCTL="$fake_systemctl" \
SYSTEMCTL_LOG="$systemctl_log" \
SYSTEMCTL_FAIL_IF_CALLED=true \
    sh "$UNINSTALL_SCRIPT" >"$stage_output" 2>&1
assert_absent "$installed_binary"
assert_absent "$installed_unit"
assert_file "$config_file"
assert_file "$password_file"
[ "$(cksum <"$config_file")" = "$config_checksum" ] ||
    fail "default uninstall changed configuration"
[ "$(cksum <"$password_file")" = "$password_checksum" ] ||
    fail "default uninstall changed the password"
assert_secret_absent "$secret_canary" "$stage_output" "$systemctl_log"
[ ! -s "$systemctl_log" ] || fail "staged uninstall invoked systemctl"

test_phase=staging-purge
DESTDIR="$stage" sh "$INSTALL_SCRIPT" --binary "$staging_binary" >/dev/null
DESTDIR="$stage" sh "$UNINSTALL_SCRIPT" --purge-config >/dev/null
assert_absent "$installed_binary"
assert_absent "$installed_unit"
assert_absent "$config_file"
assert_absent "$password_file"
[ ! -d "$config_dir" ] || fail "empty configuration directory was not removed"

for rejected_destdir in / /./; do
    expect_failure "explicit root DESTDIR" "$scratch/root-destdir.out" \
        env DESTDIR="$rejected_destdir" sh "$INSTALL_SCRIPT" --binary "$staging_binary"
    expect_failure "explicit root DESTDIR for uninstall" "$scratch/root-destdir.out" \
        env DESTDIR="$rejected_destdir" sh "$UNINSTALL_SCRIPT"
done
ln -s -- / "$scratch/root-link"
expect_failure "DESTDIR resolving to root" "$scratch/root-destdir.out" \
    env DESTDIR="$scratch/root-link" sh "$INSTALL_SCRIPT" --binary "$staging_binary"
expect_failure "relative DESTDIR" "$scratch/root-destdir.out" \
    env DESTDIR=relative sh "$INSTALL_SCRIPT" --binary "$staging_binary"

test_intermediate_symlink() {
    relative_path=$1
    case_name=$(printf '%s' "$relative_path" | tr / _)
    attack_stage="$scratch/intermediate-$case_name"
    victim="$scratch/victim-$case_name"
    attack_path="$attack_stage/$relative_path"
    mkdir -p -- "$(dirname -- "$attack_path")" "$victim"
    printf 'outside-canary\n' >"$victim/canary"
    ln -s -- "$victim" "$attack_path"

    expect_failure "installer intermediate symlink $relative_path" \
        "$scratch/intermediate.out" \
        env DESTDIR="$attack_stage" sh "$INSTALL_SCRIPT" --binary "$staging_binary"
    expect_failure "uninstaller intermediate symlink $relative_path" \
        "$scratch/intermediate.out" \
        env DESTDIR="$attack_stage" sh "$UNINSTALL_SCRIPT" --purge-config
    [ "$(find "$victim" -mindepth 1 -maxdepth 1 | wc -l)" -eq 1 ] ||
        fail "managed operation escaped through $relative_path"
    grep -F -x outside-canary "$victim/canary" >/dev/null ||
        fail "outside canary changed through $relative_path"
}

for intermediate_path in \
    usr \
    usr/local \
    usr/local/bin \
    etc \
    etc/rpi-health-mqtt \
    etc/systemd \
    etc/systemd/system; do
    test_intermediate_symlink "$intermediate_path"
done

test_final_dangling_symlink() {
    relative_path=$1
    case_name=$(printf '%s' "$relative_path" | tr / _)
    attack_stage="$scratch/final-$case_name"
    attack_path="$attack_stage/$relative_path"
    mkdir -p -- "$(dirname -- "$attack_path")"
    ln -s -- "$scratch/missing-$case_name" "$attack_path"

    expect_failure "installer dangling symlink $relative_path" \
        "$scratch/final.out" \
        env DESTDIR="$attack_stage" sh "$INSTALL_SCRIPT" --binary "$staging_binary"
    expect_failure "uninstaller dangling symlink $relative_path" \
        "$scratch/final.out" \
        env DESTDIR="$attack_stage" sh "$UNINSTALL_SCRIPT" --purge-config
}

for final_path in \
    usr/local/bin/rpi-health-mqtt \
    etc/systemd/system/rpi-health-mqtt.service \
    etc/rpi-health-mqtt/config.toml \
    etc/rpi-health-mqtt/mqtt-password; do
    test_final_dangling_symlink "$final_path"
done

hardlink_stage="$scratch/hardlink stage"
mkdir -p -- "$hardlink_stage"
test_phase=hardlink-test-setup
DESTDIR="$hardlink_stage" sh "$INSTALL_SCRIPT" --binary "$staging_binary" >/dev/null
hardlink_config="$hardlink_stage/etc/rpi-health-mqtt/config.toml"
hardlink_password="$hardlink_stage/etc/rpi-health-mqtt/mqtt-password"
hardlink_binary="$hardlink_stage/usr/local/bin/rpi-health-mqtt"
hardlink_unit="$hardlink_stage/etc/systemd/system/rpi-health-mqtt.service"

rm -- "$hardlink_config"
printf 'external configuration\n' >"$scratch/external-config"
chmod 0777 -- "$scratch/external-config"
ln -- "$scratch/external-config" "$hardlink_config"
external_config_checksum=$(cksum <"$scratch/external-config")
expect_failure "hard-linked configuration install" "$scratch/hardlink.out" \
    env DESTDIR="$hardlink_stage" sh "$INSTALL_SCRIPT" --binary "$staging_binary"
expect_failure "hard-linked configuration purge" "$scratch/hardlink.out" \
    env DESTDIR="$hardlink_stage" sh "$UNINSTALL_SCRIPT" --purge-config
[ "$(cksum <"$scratch/external-config")" = "$external_config_checksum" ] ||
    fail "hard-linked external configuration was changed"
assert_mode "$scratch/external-config" 777
rm -- "$hardlink_config"
install -m 0640 -- "$PROJECT_ROOT/config/config.example.toml" "$hardlink_config"

printf '%s\n' "$secret_canary" >"$scratch/external-password"
chmod 0777 -- "$scratch/external-password"
ln -- "$scratch/external-password" "$hardlink_password"
external_password_checksum=$(cksum <"$scratch/external-password")
expect_failure "hard-linked password install" "$scratch/hardlink.out" \
    env DESTDIR="$hardlink_stage" sh "$INSTALL_SCRIPT" --binary "$staging_binary"
expect_failure "hard-linked password purge" "$scratch/hardlink.out" \
    env DESTDIR="$hardlink_stage" sh "$UNINSTALL_SCRIPT" --purge-config
[ "$(cksum <"$scratch/external-password")" = "$external_password_checksum" ] ||
    fail "hard-linked external password was changed"
assert_mode "$scratch/external-password" 777
assert_secret_absent "$secret_canary" "$scratch/hardlink.out"
rm -- "$hardlink_password"

ln -- "$hardlink_binary" "$scratch/external-binary-link"
expect_failure "hard-linked installed binary replacement" "$scratch/hardlink.out" \
    env DESTDIR="$hardlink_stage" sh "$INSTALL_SCRIPT" --binary "$staging_binary"
expect_failure "hard-linked installed binary removal" "$scratch/hardlink.out" \
    env DESTDIR="$hardlink_stage" sh "$UNINSTALL_SCRIPT"
rm -- "$scratch/external-binary-link"

ln -- "$hardlink_unit" "$scratch/external-unit-link"
expect_failure "hard-linked unit replacement" "$scratch/hardlink.out" \
    env DESTDIR="$hardlink_stage" sh "$INSTALL_SCRIPT" --binary "$staging_binary"
expect_failure "hard-linked unit removal" "$scratch/hardlink.out" \
    env DESTDIR="$hardlink_stage" sh "$UNINSTALL_SCRIPT"
rm -- "$scratch/external-unit-link"
DESTDIR="$hardlink_stage" sh "$UNINSTALL_SCRIPT" --purge-config >/dev/null

empty_override_stage="$scratch/empty systemctl staging"
mkdir -p -- "$empty_override_stage"
test_phase=empty-systemctl-staging
DESTDIR="$empty_override_stage" SYSTEMCTL='' \
    sh "$INSTALL_SCRIPT" --binary "$staging_binary" >/dev/null
DESTDIR="$empty_override_stage" SYSTEMCTL='' \
    sh "$UNINSTALL_SCRIPT" --purge-config >/dev/null

invalid_binary="$scratch/not-an-arm-binary"
printf '#!/bin/sh\nexit 0\n' >"$invalid_binary"
chmod 0755 -- "$invalid_binary"
invalid_binary_stage="$scratch/invalid binary stage"
mkdir -p -- "$invalid_binary_stage"
expect_failure "non-ELF staging binary" "$scratch/invalid-binary.out" \
    env DESTDIR="$invalid_binary_stage" sh "$INSTALL_SCRIPT" --binary "$invalid_binary"
assert_absent "$invalid_binary_stage/usr/local/bin/rpi-health-mqtt"

soft_float_source="$scratch/soft-float.S"
soft_float_binary="$scratch/soft-float-armv7"
test_phase=soft-float-fixture
printf '%s\n' \
    '.syntax unified' \
    '.global _start' \
    '_start:' \
    '    mov r7, #1' \
    '    mov r0, #0' \
    '    svc #0' >"$soft_float_source"
arm-linux-gnueabihf-gcc \
    -march=armv7-a \
    -mfloat-abi=softfp \
    -nostdlib \
    -static \
    -Wl,-e,_start \
    -o "$soft_float_binary" \
    "$soft_float_source"
expect_failure "soft-float staging binary" "$scratch/invalid-binary.out" \
    env DESTDIR="$invalid_binary_stage" sh "$INSTALL_SCRIPT" --binary "$soft_float_binary"

install -m 0755 -- "$TEST_DIR/fake-uname.sh" "$fake_uname_path"
install -m 0755 -- "$TEST_DIR/fake-dpkg.sh" "$fake_dpkg_path"

live_environment() {
    env \
        INSTALL_TEST_UNAME_MACHINE=armv7l \
        INSTALL_TEST_DPKG_ARCHITECTURE=armhf \
        SYSTEMCTL="$fake_systemctl" \
        SYSTEMCTL_LOG="$systemctl_log" \
        "$@"
}

expect_failure "unsupported live kernel" "$scratch/platform.out" \
    env \
        INSTALL_TEST_UNAME_MACHINE=x86_64 \
        INSTALL_TEST_DPKG_ARCHITECTURE=armhf \
        SYSTEMCTL="$fake_systemctl" \
        SYSTEMCTL_LOG="$systemctl_log" \
        sh "$INSTALL_SCRIPT" --binary "$staging_binary"
expect_failure "unsupported live userland" "$scratch/platform.out" \
    env \
        INSTALL_TEST_UNAME_MACHINE=armv7l \
        INSTALL_TEST_DPKG_ARCHITECTURE=amd64 \
        SYSTEMCTL="$fake_systemctl" \
        SYSTEMCTL_LOG="$systemctl_log" \
        sh "$INSTALL_SCRIPT" --binary "$staging_binary"
expect_failure "empty live SYSTEMCTL override" "$scratch/platform.out" \
    env \
        INSTALL_TEST_UNAME_MACHINE=armv7l \
        INSTALL_TEST_DPKG_ARCHITECTURE=armhf \
        SYSTEMCTL= \
        sh "$INSTALL_SCRIPT" --binary "$staging_binary"
expect_failure "empty live SYSTEMCTL override on uninstall" "$scratch/platform.out" \
    env SYSTEMCTL= sh "$UNINSTALL_SCRIPT"

if ! getent group video >/dev/null 2>&1; then
    groupadd --system video
    video_group_created=true
else
    video_group_created=false
fi

test_phase=account-collision-tests
groupadd --system rpi-health-mqtt
gpasswd --add daemon rpi-health-mqtt >/dev/null
expect_failure "service group with unexpected member" "$scratch/account.out" \
    live_environment sh "$INSTALL_SCRIPT" --binary "$staging_binary"
account_cleanup

daemon_gid=$(getent group daemon | awk -F: '{ print $3 }')
groupadd --system --non-unique --gid "$daemon_gid" rpi-health-mqtt
expect_failure "service group with shared numeric ID" "$scratch/account.out" \
    live_environment sh "$INSTALL_SCRIPT" --binary "$staging_binary"
account_cleanup

groupadd --gid 20000 rpi-health-mqtt
expect_failure "service group with non-system GID" "$scratch/account.out" \
    live_environment sh "$INSTALL_SCRIPT" --binary "$staging_binary"
account_cleanup

groupadd --system rpi-health-mqtt
daemon_uid=$(getent passwd daemon | awk -F: '{ print $3 }')
useradd \
    --system \
    --non-unique \
    --uid "$daemon_uid" \
    --gid rpi-health-mqtt \
    --groups video \
    --home-dir /nonexistent \
    --no-create-home \
    --shell /usr/sbin/nologin \
    rpi-health-mqtt
expect_failure "service user with shared numeric ID" "$scratch/account.out" \
    live_environment sh "$INSTALL_SCRIPT" --binary "$staging_binary"
account_cleanup

useradd \
    --system \
    --no-user-group \
    --gid nogroup \
    --groups video \
    --home-dir /nonexistent \
    --no-create-home \
    --shell /usr/sbin/nologin \
    rpi-health-mqtt
expect_failure "service user without dedicated group" "$scratch/account.out" \
    live_environment sh "$INSTALL_SCRIPT" --binary "$staging_binary"
account_cleanup

groupadd --system rpi-health-mqtt
useradd \
    --system \
    --gid rpi-health-mqtt \
    --groups video \
    --home-dir /var/lib/rpi-health-mqtt \
    --no-create-home \
    --shell /usr/sbin/nologin \
    rpi-health-mqtt
expect_failure "service user with unexpected home" "$scratch/account.out" \
    live_environment sh "$INSTALL_SCRIPT" --binary "$staging_binary"
account_cleanup

groupadd --system rpi-health-mqtt
useradd \
    --system \
    --gid rpi-health-mqtt \
    --groups video,daemon \
    --home-dir /nonexistent \
    --no-create-home \
    --shell /usr/sbin/nologin \
    rpi-health-mqtt
expect_failure "service user with extra group" "$scratch/account.out" \
    live_environment sh "$INSTALL_SCRIPT" --binary "$staging_binary"
account_cleanup

groupadd --system rpi-health-mqtt
useradd \
    --system \
    --gid rpi-health-mqtt \
    --home-dir /nonexistent \
    --no-create-home \
    --shell /usr/sbin/nologin \
    rpi-health-mqtt
expect_failure "service user without video group" "$scratch/account.out" \
    live_environment sh "$INSTALL_SCRIPT" --binary "$staging_binary"
account_cleanup

groupadd --system rpi-health-mqtt
useradd \
    --system \
    --no-user-group \
    --gid nogroup \
    --groups video \
    --home-dir /nonexistent \
    --no-create-home \
    --shell /usr/sbin/nologin \
    rpi-health-mqtt
expect_failure "service user with wrong primary group" "$scratch/account.out" \
    live_environment sh "$INSTALL_SCRIPT" --binary "$staging_binary"
account_cleanup

groupadd --system rpi-health-mqtt
useradd \
    --system \
    --gid rpi-health-mqtt \
    --groups video \
    --home-dir /nonexistent \
    --no-create-home \
    --shell /bin/sh \
    rpi-health-mqtt
expect_failure "service user with interactive shell" "$scratch/account.out" \
    live_environment sh "$INSTALL_SCRIPT" --binary "$staging_binary"
account_cleanup

groupadd --system rpi-health-mqtt
useradd \
    --system \
    --uid 20000 \
    --gid rpi-health-mqtt \
    --groups video \
    --home-dir /nonexistent \
    --no-create-home \
    --shell /usr/sbin/nologin \
    rpi-health-mqtt
expect_failure "service user with non-system UID" "$scratch/account.out" \
    live_environment sh "$INSTALL_SCRIPT" --binary "$staging_binary"
account_cleanup

groupadd --system rpi-health-mqtt
useradd \
    --system \
    --gid rpi-health-mqtt \
    --groups video \
    --home-dir /nonexistent \
    --no-create-home \
    --shell /usr/sbin/nologin \
    rpi-health-mqtt

: >"$systemctl_log"
live_install_output="$scratch/live-install.out"
test_phase=existing-account-live-install
live_environment sh "$INSTALL_SCRIPT" --binary "$staging_binary" \
    >"$live_install_output" 2>&1
assert_file "$live_binary"
assert_file "$live_unit"
assert_file "$live_config_dir/config.toml"
assert_absent "$live_config_dir/mqtt-password"
assert_mode "$live_binary" 755
assert_mode "$live_unit" 644
assert_mode "$live_config_dir" 750
assert_mode "$live_config_dir/config.toml" 640
assert_owner "$live_binary" root:root
assert_owner "$live_config_dir/config.toml" root:rpi-health-mqtt
assert_log_line "$systemctl_log" "daemon-reload"
if grep -F -x "enable rpi-health-mqtt.service" "$systemctl_log" >/dev/null; then
    fail "live installer enabled the service without a password"
fi

service_group_entry=$(getent group rpi-health-mqtt)
[ "$(printf '%s\n' "$service_group_entry" | awk -F: '{ print $4 }')" = "" ] ||
    fail "service group acquired explicit members"
[ "$(getent passwd rpi-health-mqtt | awk -F: '{ print $6 }')" = /nonexistent ] ||
    fail "service user home is not /nonexistent"
case "$(getent passwd rpi-health-mqtt | awk -F: '{ print $7 }')" in
    /usr/sbin/nologin|/sbin/nologin|/bin/false)
        ;;
    *)
        fail "service user has an unexpected login shell"
        ;;
esac

printf '%s\n' "$secret_canary" >"$live_config_dir/mqtt-password"
chmod 0777 -- "$live_config_dir/config.toml" "$live_config_dir/mqtt-password"
live_config_checksum=$(cksum <"$live_config_dir/config.toml")
live_password_checksum=$(cksum <"$live_config_dir/mqtt-password")
: >"$systemctl_log"
test_phase=idempotent-live-install
live_environment sh "$INSTALL_SCRIPT" --binary "$staging_binary" \
    >"$live_install_output" 2>&1
[ "$(cksum <"$live_config_dir/config.toml")" = "$live_config_checksum" ] ||
    fail "live reinstall changed configuration"
[ "$(cksum <"$live_config_dir/mqtt-password")" = "$live_password_checksum" ] ||
    fail "live reinstall changed password"
assert_mode "$live_config_dir/config.toml" 640
assert_mode "$live_config_dir/mqtt-password" 640
assert_owner "$live_config_dir/mqtt-password" root:rpi-health-mqtt
assert_log_line "$systemctl_log" "enable rpi-health-mqtt.service"
assert_log_line "$systemctl_log" "restart rpi-health-mqtt.service"
assert_secret_absent "$secret_canary" "$live_install_output" "$systemctl_log"

live_environment sh "$INSTALL_SCRIPT" --binary "$staging_binary" \
    >"$live_install_output" 2>&1
[ "$(cksum <"$live_config_dir/config.toml")" = "$live_config_checksum" ] ||
    fail "second live reinstall changed configuration"
[ "$(cksum <"$live_config_dir/mqtt-password")" = "$live_password_checksum" ] ||
    fail "second live reinstall changed password"

: >"$systemctl_log"
expect_failure "active service stop failure" "$scratch/stop.out" \
    live_environment env \
        SYSTEMCTL_DISABLE_RESULT=failure \
        SYSTEMCTL_LOAD_STATE=loaded \
        SYSTEMCTL_ACTIVE_STATE=active \
        sh "$UNINSTALL_SCRIPT"
assert_file "$live_binary"
assert_file "$live_unit"

expect_failure "failed service is not verified inactive" "$scratch/stop.out" \
    live_environment env \
        SYSTEMCTL_DISABLE_RESULT=failure \
        SYSTEMCTL_LOAD_STATE=loaded \
        SYSTEMCTL_ACTIVE_STATE=failed \
        sh "$UNINSTALL_SCRIPT"
assert_file "$live_binary"
assert_file "$live_unit"

expect_failure "unverifiable service stop failure" "$scratch/stop.out" \
    live_environment env \
        SYSTEMCTL_DISABLE_RESULT=failure \
        SYSTEMCTL_SHOW_RESULT=failure \
        sh "$UNINSTALL_SCRIPT"
assert_file "$live_binary"
assert_file "$live_unit"

test_phase=verified-inactive-live-uninstall
live_environment env \
    SYSTEMCTL_DISABLE_RESULT=failure \
    SYSTEMCTL_LOAD_STATE=loaded \
    SYSTEMCTL_ACTIVE_STATE=inactive \
    sh "$UNINSTALL_SCRIPT" >"$scratch/stop.out" 2>&1
assert_absent "$live_binary"
assert_absent "$live_unit"
assert_file "$live_config_dir/config.toml"
assert_file "$live_config_dir/mqtt-password"
assert_secret_absent "$secret_canary" "$scratch/stop.out" "$systemctl_log"
[ "$(cksum <"$live_config_dir/config.toml")" = "$live_config_checksum" ] ||
    fail "live uninstall changed configuration"
[ "$(cksum <"$live_config_dir/mqtt-password")" = "$live_password_checksum" ] ||
    fail "live uninstall changed password"
getent passwd rpi-health-mqtt >/dev/null ||
    fail "live uninstall removed the service user"
getent group rpi-health-mqtt >/dev/null ||
    fail "live uninstall removed the service group"

live_environment sh "$INSTALL_SCRIPT" --binary "$staging_binary" >/dev/null 2>&1
live_environment env \
    SYSTEMCTL_DISABLE_RESULT=failure \
    SYSTEMCTL_LOAD_STATE=not-found \
    SYSTEMCTL_ACTIVE_STATE=inactive \
    sh "$UNINSTALL_SCRIPT" >/dev/null 2>&1
assert_absent "$live_binary"
assert_absent "$live_unit"
live_environment env \
    SYSTEMCTL_DISABLE_RESULT=failure \
    SYSTEMCTL_LOAD_STATE=not-found \
    SYSTEMCTL_ACTIVE_STATE=inactive \
    sh "$UNINSTALL_SCRIPT" --purge-config >/dev/null 2>&1
assert_absent "$live_config_dir/config.toml"
assert_absent "$live_config_dir/mqtt-password"
[ ! -d "$live_config_dir" ] || fail "live purge left an empty configuration directory"

test_phase=clean-account-live-install
account_cleanup
live_environment sh "$INSTALL_SCRIPT" --binary "$staging_binary" >/dev/null 2>&1
getent passwd rpi-health-mqtt >/dev/null ||
    fail "clean install did not create the service user"
getent group rpi-health-mqtt >/dev/null ||
    fail "clean install did not create the service group"
live_environment sh "$INSTALL_SCRIPT" --binary "$staging_binary" >/dev/null 2>&1
live_environment sh "$UNINSTALL_SCRIPT" --purge-config >/dev/null 2>&1
getent passwd rpi-health-mqtt >/dev/null ||
    fail "uninstall did not preserve the installer-created service user"
getent group rpi-health-mqtt >/dev/null ||
    fail "uninstall did not preserve the installer-created service group"

live_environment env \
    SYSTEMCTL_DISABLE_RESULT=failure \
    SYSTEMCTL_LOAD_STATE=not-found \
    SYSTEMCTL_ACTIVE_STATE=inactive \
    sh "$UNINSTALL_SCRIPT" >/dev/null 2>&1

grep -F -x "SupplementaryGroups=video" \
    "$PROJECT_ROOT/systemd/rpi-health-mqtt.service" >/dev/null ||
    fail "systemd unit does not grant video group access"
grep -F -x "NoNewPrivileges=true" \
    "$PROJECT_ROOT/systemd/rpi-health-mqtt.service" >/dev/null ||
    fail "systemd unit is missing NoNewPrivileges"
if grep -E '^[[:space:]]*PrivateDevices=' \
    "$PROJECT_ROOT/systemd/rpi-health-mqtt.service" >/dev/null; then
    fail "systemd unit may block access to /dev/vcio"
fi

printf 'Installer tests passed.\n'
test_phase=complete
