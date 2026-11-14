# SPDX-License-Identifier: GPL-3.0-or-later
# shellcheck shell=bash
#
# The server container's privileges in the CI tests: the documented set, the same as in
# deploy/compose.yaml and deploy/nas-compose.yaml (docs/deployment.md, "Capabilities", #386).
# None of the runtime's default capabilities, only these; no gaining privileges through setuid
# files; AppArmor's default profile forbids mount(2), so it is off.
# shellcheck disable=SC2034  # used by the scripts that source this file
SERVER_OPTS=(--device /dev/fuse --cap-drop ALL
    --cap-add SYS_ADMIN --cap-add DAC_OVERRIDE --cap-add DAC_READ_SEARCH --cap-add SETUID
    --cap-add SETGID --cap-add NET_BIND_SERVICE --cap-add SETPCAP
    --security-opt no-new-privileges:true --security-opt apparmor=unconfined)
