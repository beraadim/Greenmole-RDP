# macrdp 0.1.9

Release candidate: 2026-10-13. This release restores double-click recognition
for Guacamole and other RDP clients on macOS.

## Mouse clicks

RDP sends individual mouse-button transitions. The server previously posted
each transition as a new CoreGraphics event without an explicit click count,
so macOS applications could not recognize the second click as a double-click.
The injector now supplies consistent click counts on ordinary mouse-down and
mouse-up events, including triple-clicks for text selection. Prolonged releases
report zero, following AppKit's click-count behavior.

Click recognition reads the Mac's double-click interval on each press, so changes
to the host setting take effect immediately. The server permits pointer jitter
within four logical display points of the first click. Movement beyond that
radius, button chords, slow clicks and session cleanup break a sequence.
Duplicate button transitions are ignored. Failed event creation preserves the
held-button state for retry. Trace-level mouse logs include the posted count.

The existing fast-path, slow-path and additional-input dispatchers keep their
ordered position and button delivery. Guacamole does not need a separate
double-click setting for this fix.

## Acceptance checks

- Reconnect Guacamole after installing the release. Double-click a Finder item
  to open it, and double-click text to select a word.
- Triple-click text in an application that supports paragraph or line selection.
- Check single clicks, drag selection and window dragging. Slow clicks and clicks
  on different items should remain separate.
- Disconnect while holding a button, then reconnect. The first click in the new
  session should be a single click, with no stuck drag.
- Repeat ordinary clicks, double-clicks and dragging with Windows Remote Desktop.

## Automated validation

Validation runs on the Apple Silicon Mac mini. Mouse tests use recorded event
sinks and construct native CoreGraphics events without posting clicks to the
desktop. They cover click counts, timing and movement boundaries, button
independence, duplicate transitions, failure retries and session cleanup.

All checks passed: 488 workspace Rust tests, 38 desktop backend Rust tests,
eight Python tests and eight JavaScript tests (542 total). Four optional
microbenchmarks remain excluded. CLI and TypeScript/Vite production builds
passed. The native preference reader also passed a separate worker-thread test.
End-to-end application behavior remains for user acceptance.

## Native API references

- [CoreGraphics mouse click state](https://developer.apple.com/documentation/coregraphics/cgeventfield/mouseeventclickstate)
- [AppKit double-click interval](https://developer.apple.com/documentation/appkit/nsevent/doubleclickinterval)
- [AppKit release click counts](https://developer.apple.com/documentation/appkit/nsevent/clickcount)
