# Security

Adytum keeps sealed cells: a VM (Lima) or a container (podman, docker) per machine, with walls between them, a ledger that survives a kill, and commands that never go through a shell. If you find a way to make Adytum cross a boundary it should hold, tell Will before telling anyone else.

## Reporting a problem

Email will@willctl.com. Don't open a public issue or pull request for a security problem, so the fix can be out before the problem is.

Say what you found, how to reproduce it, which version you ran (`adytum --version`) and which operating system. Say what had to be running already, under which account, and what it could read or change. A proof of concept helps, as text or a short script.

You'll get an answer within seven days. Adytum is at 0.x: fixes go into the latest release only.

## What's in scope

Everything in this repository: the `adytum` command and the crates under `crates/`.
