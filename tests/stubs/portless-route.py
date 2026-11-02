"""The part of the portless stand-in that owns a route: it runs the command
with PORT set, lists the route while it runs, and removes it on SIGINT. Python
rather than sh, since a backgrounded sh can't trap SIGINT."""

import os
import signal
import socket
import subprocess
import sys

here = os.path.dirname(os.path.abspath(__file__))
name, command = sys.argv[1], sys.argv[2:]
if command[:1] == ["--force"]:
    command = command[1:]

with socket.socket() as probe:
    probe.bind(("127.0.0.1", 0))
    port = probe.getsockname()[1]
tailnet = ""
if os.environ.get("PORTLESS_TAILSCALE") == "1":
    tailnet = f"https://host.example.ts.net:{port}"

routes = os.path.join(here, "routes")
os.makedirs(routes, exist_ok=True)
route = os.path.join(routes, name + ".route")
child = subprocess.Popen(command, env={**os.environ, "PORT": str(port)})
with open(route, "w") as file:
    file.write(f"{name} {port} {os.getpid()} {tailnet}\n")


# On SIGINT, stop the command; the route goes once it has exited.
signal.signal(signal.SIGINT, lambda *_: child.terminate())
child.wait()
os.remove(route)
