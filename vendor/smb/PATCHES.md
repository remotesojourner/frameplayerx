# smb 0.12.1, patched

This is [smb](https://crates.io/crates/smb) 0.12.1 (smb-rs,
https://github.com/afiffon/smb-rs, MIT, see LICENSE.md) as published on
crates.io, used through `[patch.crates-io]` in the workspace `Cargo.toml`.
The tests and dev-dependencies are left out; everything else is unchanged
except for the SMB credit accounting below, guest sign-in (see
[Guest sign-in](#guest-sign-in)) and two compiler warnings (see
[Warnings](#warnings)). The same code is still on
upstream `main` (f4524d0) as of October 2026. Drop this copy once a release
fixes it.

## The bug

SMB2 servers hand out credits; each request spends some (a 1 MiB read spends
16) and each response grants some back. smb-rs counts its credits in a
semaphore, and a request waits on it until enough credits are free.

- When a request takes the server a while (Samba: a read not done within
  0.5 s, which happens with a busy NAS or a spun-down disk), the server first
  sends an interim `STATUS_PENDING` response, which grants the credits, then
  the final response, which Samba sends with no credits. smb-rs only counted
  the final response, so every read that went async lost its credits for
  good.
- The pool size then dropped below zero (a `u16` that wrapped around), after
  which the client stopped asking the server for more credits.
- The wait on the semaphore had no timeout.

So after enough slow reads every request on the connection, the read in
progress and every later open, waited forever. FramePlayerX showed this as
playback from a NAS freezing on "buffering" after a few minutes, with no
other video opening until the app was restarted. A Samba share served from a
FUSE folder that delays one read in 40 by 1.2 s hung the old code within
5 MiB; the patched code reads the whole file.

## The changes (marked "FramePlayerX patch" in the source)

- `src/connection/worker/worker_trait.rs`: credits granted by interim
  responses are added to the final response's credit grant, so the credit
  accounting sees them.
- `src/connection.rs`: the pool size update saturates instead of wrapping,
  and a request waits for credits at most the connection timeout, then fails
  with `Error::OperationTimeout(TimedOutTask::AcquireCredits, _)` (FramePlayerX
  then reconnects).
- `src/sync_helpers.rs`: `Semaphore::acquire_many_timeout`.
- `src/error.rs`: `TimedOutTask::AcquireCredits`.

## Guest sign-in

A server that takes a login as a guest (Samba's default `map to guest = Bad
User`, for a client with no account on it) answers the last session setup
with success and the guest flag, but with no final SPNEGO token: a guest
session has no key to sign one with. smb-rs waits for that token to finish
SSPI and failed every such sign-in with "SMB session setup succeeded before
SSPI authentication completed", so an SMB source with no login reached no
share on such a server.

- `src/session/setup.rs` (`complete_authentication`): a success reply
  flagged guest or anonymous finishes the setup without the token when the
  connection allows unsigned guest sessions (`allow_unsigned_guest_access`,
  which FramePlayerX sets only for a source with no login). With a login,
  the token is still required, so a real sign-in can't be turned into a
  guest one.

## Warnings

- `src/resource/file_util.rs`: the `sync_helpers` import moved into the
  `std-fs-impls` module, its only user (unused in the sync build).
- `src/connection.rs`: the credit pool update uses `update` (Rust 1.95)
  instead of `fetch_update`, deprecated from Rust 1.99; `rust-version` in
  `Cargo.toml` is raised from 1.89.0 to 1.95.0 to match.
