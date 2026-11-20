# Adytum: notes for agents

- Read [docs/design.md](docs/design.md) before changing a rule, and [README.md](README.md) for the commands.
- Checks are in [CONTRIBUTING.md](CONTRIBUTING.md). Live tests (`ADYTUM_LIVE=1`) make real VMs named `adytum-test-*`; never start, stop or delete another VM.
- A new refusal gets its test first. No secret in an argument, a temporary file or a log.
- Exit codes: 0 done, 1 refused or failed, 2 misuse. The `--json` error codes are a contract.
