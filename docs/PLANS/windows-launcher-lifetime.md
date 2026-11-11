# Windows launcher process lifetime

Work tracking (2026-11-11): **Done (scoped contract)**. Remaining product work: [current TODO](../TODO.md#platform-distribution-and-native-acceptance).

Implemented contract. Native development artifacts reproduce
an owned server surviving forced launcher termination and continuing to answer
`/v1/ready`. Desktop lease cleanup kills the launcher process, so a launcher
must own its descendants even when normal Rust cleanup cannot execute.

On Windows, create an unnamed job with KILL_ON_JOB_CLOSE and a non-inheritable
handle. Assign the current launcher to it before spawning migration or server
processes. Children inherit membership at creation; do not enable breakaway.
Fail startup before spawning anything if job creation/configuration/assignment
fails. An outer compatible job may remain in the nested hierarchy.

Retain the sole job handle until OS process teardown. Closing it during ordinary
Rust stack cleanup would also terminate the launcher itself. Explicitly transfer
the handle out of Rust RAII after successful assignment; Windows closes it on
normal exit or forced termination. No child inherits a handle that could prolong
the job lifetime. A native prototype verified both descendant termination after
forced death and preservation of an ordinary exit code of 42.

Validate actual launcher/server binaries: fresh startup, forced launcher death,
server disappearance and closed readiness endpoint; retain migration/readiness
and native workspace gates. Linux/macOS supervision is unchanged. Job ownership
is a process-lifetime guarantee, not filesystem/network agent isolation.

References:
- https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects
- https://learn.microsoft.com/en-us/windows/win32/api/jobapi2/nf-jobapi2-assignprocesstojobobject

## Native evidence

Acceptance on 2026-10-28 reproduced the old orphan using actual development
binaries. With the job fix, killing the launcher removed the owned server and
closed its readiness endpoint. Normal launcher error exit remained 1; the
standalone native API prototype also retained exit code 42 and reaped children
on both normal and forced exits. The lifecycle fixture retains this behavior
as an actual-binary Windows regression. Native formatting, strict workspace
Clippy, workspace build, PE metadata/icon checks and all workspace tests passed:
1,611 passed, zero failed, two ignored. Existing unsigned development artifacts predate
this fix and require a fresh build before distributing the change.
