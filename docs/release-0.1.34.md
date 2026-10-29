# Greenmole RDP 0.1.34

Fixes the microphone driver's installed file permissions. Version 0.1.33 copied
the executable into a private staging directory with mode `0700` and failed to
make that executable readable before publishing the bundle. The installed
root-owned binary was therefore inaccessible to the `_coreaudiod` account,
leaving the virtual microphone unavailable even though installation succeeded.

The installer now explicitly publishes directories and the executable as `0755`
and the plist/signature resources as `0644`. It checks these modes, symlinks and
ACLs before and after publication. Digest/signature verification still runs in
the private stage before finalization. No installed driver or service is repaired
automatically: the user must choose **Settings → Microphone → Reinstall driver**
from the updated app when ready to interrupt Core Audio.

Driver status detects inaccessible or unsafe installed bundle permissions and
provides explicit updated-app/reinstallation guidance. It does not mistake
inaccessible metadata for an absent driver or request administrator access while
checking status.

An unprivileged regression test executes the shipped staging and finalization
functions against temporary files, verifies private staging and the published
permissions, and rejects the previous `0700` executable plus restricted or
writable resources/directories and symlinked files. Native protocol/real-time
tests and desktop command/status tests also run without changing the system HAL.

The microphone transport and audio format are unchanged. The corrected package
still needs a live reinstall/loading test and subsequent client/Teams acceptance.
