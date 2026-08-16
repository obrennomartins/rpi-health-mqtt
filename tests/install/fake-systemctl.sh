#!/bin/sh
set -eu

: "${SYSTEMCTL_LOG:?SYSTEMCTL_LOG must name the invocation log}"
printf '%s\n' "$*" >> "$SYSTEMCTL_LOG"

if [ "${SYSTEMCTL_FAIL_IF_CALLED:-false}" = true ]; then
    exit 97
fi

command_name=${1:-}
case "$command_name" in
    disable)
        [ "${SYSTEMCTL_DISABLE_RESULT:-success}" = success ]
        ;;
    show)
        [ "${SYSTEMCTL_SHOW_RESULT:-success}" = success ] || exit 1
        requested_property=
        for argument do
            case "$argument" in
                --property=*)
                    requested_property=${argument#--property=}
                    ;;
            esac
        done
        case "$requested_property" in
            LoadState)
                printf '%s\n' "${SYSTEMCTL_LOAD_STATE:-loaded}"
                ;;
            ActiveState)
                printf '%s\n' "${SYSTEMCTL_ACTIVE_STATE:-inactive}"
                ;;
            *)
                exit 2
                ;;
        esac
        ;;
    *)
        exit 0
        ;;
esac
