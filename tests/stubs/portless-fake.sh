#!/bin/sh
# Test stand-in for portless that runs real servers: `--version`, `list`,
# and `<name> --force <command>`. Like an installed shim, it runs the program
# that owns the route as a child, so the route's pid isn't this script's.

dir=$(dirname "$0")

case "$1" in
--version)
  echo 0.15.6
  ;;
list)
  echo "Active routes:"
  echo
  for route in "$dir"/routes/*.route; do
    [ -e "$route" ] || continue
    read -r name port pid tailnet <"$route"
    kill -0 "$pid" 2>/dev/null || continue
    printf '  \033[36mhttps://%s.localhost\033[0m  ->  localhost:%s  \033[2m(pid %s)\033[0m\n' "$name" "$port" "$pid"
    if [ -n "$tailnet" ]; then
      printf '    tailscale: %s\n' "$tailnet"
    fi
  done
  ;;
*)
  python3 "$dir/portless-route.py" "$@"
  ;;
esac
