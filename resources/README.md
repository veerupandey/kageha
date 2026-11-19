# Kageha Rust resources

The canonical bundled skills live in `skills/`. The model catalog is `models.yaml`.
These are data resources, separate from the Python package layout.

An installed composition uses `bin/kageha` and `share/kageha/`, with the built
frontend under `share/kageha/webui/`. Explicit `--resources` or
`KAGEHA_RESOURCE_DIR` selects another resource root. Installed resource lookup
never searches a compiled source-checkout path.

Some retained skills still include Python helpers or Python-specific instructions.
They remain visible migration blockers until native replacements pass acceptance;
moving those files here does not establish Python-free skill parity. The legacy
source-tree links are temporary coordination aids and block the cutover gate.
