# Manual quota reset recovery

Local patch based on `feat/codex-account-rotation` at
`df58f7eb601d18234104acad378a73abc9305e0a`.

The rotation policy used to ignore restored quota until its remembered reset
deadline. Two consecutive complete successful quota polls below the relevant
window's configured floor now clear that window's remembered exhaustion.
Polls must be at least five seconds apart. Null reset timestamps are allowed:
a newly reset account may have no active window yet. Missing/partial/invalid
readings break confirmation; an exhausted reading also cancels confirmation.
The five-hour and weekly windows recover independently. No synthetic model
requests, forced account switching, credential changes or new services are used.

Run `python3 scripts/test-rotation-policy.py` to compile and test the actual
pure production policy with rustc, without the GTK/Tauri dependency stack.
This is a focused policy check, not a full application build.

The `Linux VPS build` GitHub workflow runs the focused policy tests, builds the
frontend, runs the integrated Rust rotation tests and produces an unsigned
Linux x86_64 executable. It uploads the source commit and executable checksum
with the artifact. It does not publish a release or use signing credentials.

Build artifacts target Ubuntu 24.04, matching the VPS. Before installation,
verify SOURCE_COMMIT and SHA256SUMS and preserve the current executable for
rollback. This is a tested source baseline, not a reconstruction of unrecorded
changes in older deployed binaries. Verify CLI/socket commands, proxy requests,
rotation status and manual-reset recovery after installation.
