# Kageha process sandbox

Independent Rust library for a single child process. It does not depend on the
agent loop, WebUI or Python. macOS uses the installed Seatbelt launcher; Linux
uses an explicitly installed Bubblewrap (`bwrap`). Unsupported or unavailable
required confinement fails closed; there is no unsandboxed retry.

MCP stdio configuration:

```json
{
  "id": "local-indexer",
  "transport": "stdio",
  "command": "/opt/tools/indexer",
  "inherit_env": false,
  "sandbox": {
    "read_roots": ["/opt/tools/indexer-resources"],
    "write_roots": ["/srv/kageha/workspace"],
    "network": false
  }
}
```

Roots must exist, be absolute after canonicalization and cannot be `/`. Limits
and path escaping are enforced. System executables/libraries are readable;
additional resources and writable directories must be explicitly granted.
macOS permits file metadata and root-directory inspection for process startup,
not unrestricted file contents. Network access is an explicit per-process choice.
A sandboxed adapter must use an explicit environment. Do not place operator
requirements or credentials within directories granted writable to the adapter.

Set `require_process_sandbox: true` in the immutable policy requirements file to
reject unconfined stdio adapters, including discovery. Existing trusted adapters
remain compatible when the operator has not required confinement. The sandbox is
part of the adapter fingerprint, so changing it changes resource authority.

`cargo test --locked -p kageha-sandbox -p kageha-mcp` exercises a real confined
process, allowed workspace output, forbidden external read/write, forbidden
network access and stdio MCP operation. Native execution is verified on macOS and Linux in Docker, as root and UID 1000.
Run `bash scripts/check_linux_permissions.sh` to build an isolated source fixture
and repeat the Linux MCP, filesystem, network, process cleanup and workspace tests.
The two ignored probe entry points are launched by their enclosing tests.

Docker normally blocks nested namespace/proc creation. The fixture explicitly
adds `SYS_ADMIN`, disables its outer seccomp filtering and unmasks container system
paths so Bubblewrap can create its own restricted namespaces. This is a disposable,
networkless container without host mounts or the Docker socket; it is not a
production deployment recommendation and it never uses `--privileged`. Missing
namespace permissions cause failure, not an unsandboxed fallback.

This boundary applies to local process adapters. Attaching a sandbox to an
already-running signed-in browser is not possible through this launcher. Browser
control uses its existing consent, capability, origin, generation, receipt and
script-interpreter boundaries. SSH commands execute on the selected remote host;
remote OS confinement belongs to that host and is never implied by this library.

The same image also supplies a real disposable OpenSSH server for the retained
WebUI integration suite:

```sh
KAGEHA_REAL_SSH_IMAGE=kageha-permission-fixture:local \
KAGEHA_LOOP_BIN="$PWD/target/debug/kageha-loop" \
node crates/kageha-loop/tests/native_adapters_e2e.mjs
```

This exposes SSH only on a random loopback port, uses an ephemeral key and mounts
only disposable test data. It verifies changed-host-key rejection, denied mutation
without dispatch, actual remote command failure and hash-verified uploads through
Kageha. The container and keys are removed at completion. SSH is still a remote
execution boundary: confinement of arbitrary remote commands is the SSH host’s
responsibility.
