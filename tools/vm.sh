#!/bin/bash
# Drive the win10 QEMU/KVM session VM without touching the Linux desktop.
#   ./vm.sh shot [out.png]      screenshot
#   ./vm.sh key KEY_ESC ...     send keys
#   ./vm.sh exec "command"      run a command in the guest and print its output
set -u
export LIBVIRT_DEFAULT_URI=qemu:///session
DOM=win10

case "${1:-}" in
  shot)
    out="${2:-/tmp/vm.png}"
    virsh screenshot "$DOM" "$out" >/dev/null 2>&1 && echo "wrote $out"
    ;;
  key)
    shift
    virsh send-key "$DOM" "$@" >/dev/null 2>&1 && echo "sent $*"
    ;;
  exec)
    cmd="${2:?command required}"
    pid=$(virsh qemu-agent-command "$DOM" \
      "{\"execute\":\"guest-exec\",\"arguments\":{\"path\":\"cmd.exe\",\"arg\":[\"/c\",$(python3 -c 'import json,sys;print(json.dumps(sys.argv[1]))' "$cmd")],\"capture-output\":true}}" \
      | python3 -c 'import json,sys;print(json.load(sys.stdin)["return"]["pid"])') || exit 1
    for _ in $(seq 1 40); do
      sleep 0.5
      st=$(virsh qemu-agent-command "$DOM" "{\"execute\":\"guest-exec-status\",\"arguments\":{\"pid\":$pid}}")
      done_flag=$(python3 -c 'import json,sys;print(json.loads(sys.argv[1])["return"].get("exited",False))' "$st")
      if [ "$done_flag" = "True" ]; then
        python3 - "$st" <<'PY'
import base64, json, sys
r = json.loads(sys.argv[1])["return"]
out = base64.b64decode(r.get("out-data", "") or "").decode("utf-8", "replace")
err = base64.b64decode(r.get("err-data", "") or "").decode("utf-8", "replace")
print(out.rstrip())
if err.strip():
    print("STDERR:", err.rstrip())
print("exit:", r.get("exitcode"))
PY
        exit 0
      fi
    done
    echo "timed out waiting for $pid"
    ;;
  ps)
    cmd="${2:?command required}"
    pid=$(virsh qemu-agent-command "$DOM" \
      "{\"execute\":\"guest-exec\",\"arguments\":{\"path\":\"powershell.exe\",\"arg\":[\"-NoProfile\",\"-Command\",$(python3 -c 'import json,sys;print(json.dumps(sys.argv[1]))' "$cmd")],\"capture-output\":true}}" \
      | python3 -c 'import json,sys;print(json.load(sys.stdin)["return"]["pid"])') || exit 1
    for _ in $(seq 1 60); do
      sleep 0.5
      st=$(virsh qemu-agent-command "$DOM" "{\"execute\":\"guest-exec-status\",\"arguments\":{\"pid\":$pid}}")
      done_flag=$(python3 -c 'import json,sys;print(json.loads(sys.argv[1])["return"].get("exited",False))' "$st")
      if [ "$done_flag" = "True" ]; then
        python3 - "$st" <<'PY2'
import base64, json, sys
r = json.loads(sys.argv[1])["return"]
out = base64.b64decode(r.get("out-data", "") or "").decode("utf-8", "replace")
err = base64.b64decode(r.get("err-data", "") or "").decode("utf-8", "replace")
print(out.rstrip())
if err.strip():
    print("STDERR:", err.rstrip()[:400])
print("exit:", r.get("exitcode"))
PY2
        exit 0
      fi
    done
    echo "timed out waiting for $pid"
    ;;
  type)
    text="${2:?text required}"
    python3 - "$text" <<'PY2' > /tmp/vm_keys.txt
import sys
m = {c: "KEY_" + c.upper() for c in "abcdefghijklmnopqrstuvwxyz0123456789"}
m.update({" ": "KEY_SPACE", "\\": "KEY_BACKSLASH", ".": "KEY_DOT",
          "-": "KEY_MINUS", "/": "KEY_SLASH", ",": "KEY_COMMA",
          ":": "SHIFT:KEY_SEMICOLON", "_": "SHIFT:KEY_MINUS"})
for ch in sys.argv[1]:
    print(m.get(ch, ""))
PY2
    while IFS= read -r k; do
      [ -z "$k" ] && continue
      case "$k" in
        SHIFT:*) virsh send-key "$DOM" KEY_LEFTSHIFT "${k#SHIFT:}" >/dev/null 2>&1 ;;
        *)       virsh send-key "$DOM" "$k" >/dev/null 2>&1 ;;
      esac
      sleep 0.04
    done < /tmp/vm_keys.txt
    echo "typed ${#text} chars"
    ;;
  get)
    # Copy one guest file to Linux over the guest agent (no screenshots involved).
    src="${2:?guest path required}"
    dst="${3:?local path required}"
    handle=$(virsh qemu-agent-command "$DOM" \
      "{\"execute\":\"guest-file-open\",\"arguments\":{\"path\":$(python3 -c 'import json,sys;print(json.dumps(sys.argv[1]))' "$src"),\"mode\":\"r\"}}" \
      | python3 -c 'import json,sys;print(json.load(sys.stdin)["return"])') || exit 1
    : > "$dst"
    offset=0
    while :; do
      chunk=$(virsh qemu-agent-command "$DOM" \
        "{\"execute\":\"guest-file-read\",\"arguments\":{\"handle\":$handle,\"count\":65536}}" 2>/dev/null) || break
      python3 - "$chunk" "$dst" <<'PY2' || break
import base64, json, sys
r = json.loads(sys.argv[1])["return"]
data = base64.b64decode(r.get("buf-b64", "") or "")
with open(sys.argv[2], "ab") as f:
    f.write(data)
sys.exit(0 if data else 3)
PY2
      [ $? -eq 3 ] && break
      offset=$((offset + 65536))
      [ "$offset" -gt 209715200 ] && break
    done
    virsh qemu-agent-command "$DOM" "{\"execute\":\"guest-file-close\",\"arguments\":{\"handle\":$handle}}" >/dev/null 2>&1
    echo "$(stat -c %s "$dst") bytes -> $dst"
    ;;
  put)
    # Copy a Linux file into the guest over the guest agent.
    src="${2:?local path required}"
    dst="${3:?guest path required}"
    handle=$(virsh qemu-agent-command "$DOM" \
      "{\"execute\":\"guest-file-open\",\"arguments\":{\"path\":$(python3 -c 'import json,sys;print(json.dumps(sys.argv[1]))' "$dst"),\"mode\":\"wb\"}}" \
      | python3 -c 'import json,sys;print(json.load(sys.stdin)["return"])') || exit 1
    python3 - "$src" <<'PY2' > /tmp/vm_chunks.txt
import base64, sys
data = open(sys.argv[1], 'rb').read()
for i in range(0, len(data), 65536):
    print(base64.b64encode(data[i:i+65536]).decode())
PY2
    while IFS= read -r chunk; do
      virsh qemu-agent-command "$DOM" "{\"execute\":\"guest-file-write\",\"arguments\":{\"handle\":$handle,\"buf-b64\":\"$chunk\"}}" >/dev/null 2>&1
    done < /tmp/vm_chunks.txt
    virsh qemu-agent-command "$DOM" "{\"execute\":\"guest-file-close\",\"arguments\":{\"handle\":$handle}}" >/dev/null 2>&1
    echo "wrote $dst"
    ;;
  click)
    # click <x> <y> in guest pixels (abs pointer, usb-tablet range 0..32767)
    x="${2:?x}"; y="${3:?y}"; w="${4:-1440}"; h="${5:-900}"
    ax=$(python3 -c "print(int(int('$x')/int('$w')*32767))")
    ay=$(python3 -c "print(int(int('$y')/int('$h')*32767))")
    virsh qemu-monitor-command "$DOM" "{\"execute\":\"input-send-event\",\"arguments\":{\"events\":[{\"type\":\"abs\",\"data\":{\"axis\":\"x\",\"value\":$ax}},{\"type\":\"abs\",\"data\":{\"axis\":\"y\",\"value\":$ay}},{\"type\":\"btn\",\"data\":{\"down\":true,\"button\":\"left\"}},{\"type\":\"btn\",\"data\":{\"down\":false,\"button\":\"left\"}}]}}" >/dev/null 2>&1 && echo "clicked $x,$y"
    ;;
  *)
    echo "usage: $0 shot|key|exec|ps|type|get|put|click ..." >&2
    exit 2
    ;;
esac
