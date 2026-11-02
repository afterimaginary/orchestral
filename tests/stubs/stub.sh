#!/bin/sh
# Test stand-in for an external tool (claude, gh, portless, tailscale).
# Installed under the tool's name by tests/common/mod.rs. Records every call,
# then replies with the first canned rule whose arguments are a prefix of the
# call's. Arguments are joined with the unit separator, calls with the record
# separator, so arguments may contain spaces and newlines.

tool=$(basename "$0")
dir=$(dirname "$0")
us=$(printf '\037')
rs=$(printf '\036')

joined=""
for arg in "$@"; do
  joined="$joined$arg$us"
done
printf '%s%s' "$joined" "$rs" >>"$dir/calls/$tool"

for rule in "$dir/rules/$tool".*.args; do
  [ -e "$rule" ] || break
  # The trailing x keeps $(...) from dropping trailing newlines.
  prefix=$(cat "$rule"; printf x)
  prefix=${prefix%x}
  case "$joined" in
  "$prefix"*)
    base=${rule%.args}
    cat "$base.stdout"
    exit "$(cat "$base.code")"
    ;;
  esac
done

echo "stub $tool: no canned reply for: $*" >&2
exit 99
