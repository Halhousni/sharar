#!/bin/bash
# Run one .s2s in Spike2 inside the Windows guest and pull back the .bin it writes.
#
#   tools/oracle/run.sh t2        # runs tools/oracle/t2.s2s, fetches vmbin/oracle/t2.bin
#
# Spike2 runs startup.s2s from its install folder when it starts, so this writes a
# one-line startup.s2s that calls ScriptRun on the test, restarts Spike2 through a
# scheduled task (the guest agent's own session has no desktop), and waits for the
# output file. Needs the guest from tools/vm.sh with a task named sharar_spike that
# starts SONVIEW.exe in the user's session.
set -u
cd "$(dirname "$0")/../.."
name="${1:?test name}"
G='C:\Users\Hilal\Documents\Spike8'
VM=tools/vm.sh
mkdir -p vmbin/oracle
printf 'ScriptRun("%s\\\\%s.s2s");\r\n' "${G//\\/\\\\}" "$name" > vmbin/oracle/startup.s2s
bash $VM put "tools/oracle/$name.s2s" "$G\\$name.s2s" >/dev/null
[ -f vmbin/oracle/nan.bin ] && bash $VM put vmbin/oracle/nan.bin "$G\\nan.bin" >/dev/null
bash $VM put vmbin/oracle/startup.s2s 'C:\Program Files\CED\Spike8\startup.s2s' >/dev/null
bash $VM ps "Get-Process sonview -EA SilentlyContinue | ForEach-Object { \$_.Kill() }; Remove-Item '$G\\$name.bin' -EA SilentlyContinue; Start-Sleep 2; Start-ScheduledTask sharar_spike" >/dev/null
for _ in $(seq 1 30); do
  sleep 3
  if bash $VM get "$G\\$name.bin" "vmbin/oracle/$name.bin" >/dev/null 2>&1; then
    echo "got vmbin/oracle/$name.bin ($(stat -c %s "vmbin/oracle/$name.bin") bytes)"
    exit 0
  fi
done
echo "no output from $name: Spike2 probably stopped on a script error; screenshot with tools/vm.sh shot" >&2
exit 1
