# Release 0.1.15: reopen hidden controls and translate cut/select all

## Closing, reopening and quitting

The red close button intentionally hides macrdp's controls while its menu-bar
server continues running. macOS opening an already-running app from Finder
sends a native Reopen event rather than starting a second process. Version
0.1.14 handled only the second-process notification, so closing the controls
could leave the running app inaccessible until it was force-quit.

The application now handles the native Reopen event and the second-instance
notification through the same reveal path. Reopen always shows the main window,
including when a visible tray popover makes macOS report that the app already
has a visible window. It unhides the application, restores a minimized window,
shows it, and focuses it. Display placement failures are logged but do not
suppress the visibility actions.

Ordinary Open/Reopen follows the monitor under the pointer, including the remote
virtual display. Forcing ordinary Open onto the physical monitor could also make
the controls disappear from a remote user. Emergency local desktop recovery
continues to request a physical monitor. Window reveal uses AppKit on its main
thread, with the existing bounded cancellation guard; it does not acquire the
server's lifecycle lock, stop the service or alter display ownership.

The native tray menu now offers **Quit macrdp and stop server**. Native Quit,
the tray item and the popover's Quit button share one supervised shutdown path.
Stop intent is recorded before waiting for service transitions; duplicate Quit
requests do not start additional cleanup, and new server starts are rejected
once quitting begins. The application permits its final exit after supervised
shutdown completes or reaches the existing bounded recovery deadline. This
also covers a cancelled startup that has not yet published a server handle.

## Keyboard shortcuts

Remote Ctrl+X now maps to Command+X (cut), and Ctrl+A maps to Command+A (select
all), alongside the existing Ctrl+C, Ctrl+V and Ctrl+Enter mappings. Both left
and right Ctrl work, with Guacamole and Windows Remote Desktop Connection.
These use the existing explicit native modifier restoration so temporary
Command flags cannot remain held after the shortcut.

The original mapping is retained through key repeats and key-up even if Ctrl
is released first or client input context changes. Shift variants preserve
Shift. Ctrl+Alt, AltGr and Windows-key combinations retain their existing
behavior. Normal A/X text and unrelated Control combinations are unchanged.
Disconnect, reconnect and synchronize cleanup release only owned shortcut keys.

## Validation and installation acceptance

Shortcut tests cover both Ctrl keys, clients, US/Norwegian layouts, repeats,
release order, following text, AltGr exclusion and session cleanup. Native event
tests inspect Command restoration without posting input to the live desktop.
Window tests cover negative display origins, a pointer on the remote screen,
screen edges, invalid geometry, and revealing controls after placement errors.
The Quit test exercises concurrent requests and the final-exit gate.

`tools/check.sh` passed: 658 Rust tests, eight Python tests and eight JavaScript
tests; six existing opt-in experiments remained ignored. The CLI build,
TypeScript check and production frontend build passed. The locked Tauri backend
check passed separately. These checks did not post keyboard input or alter the
running server, virtual display or installed application. The native interaction
checks below remain for installation testing.

After installing the Desktop DMG, close the main window with the red button,
then reopen macrdp from Finder and from **Open macrdp** in its tray menu. Check
this both locally and through the remote display, including a minimized window
and a visible popover. The remote session should remain connected. Test the
native Quit item separately: the server and its owned virtual display should
stop. Reconnect after relaunch and test Ctrl+A, Ctrl+X and Ctrl+V in a text editor;
ordinary typing afterwards should not require pressing Alt to clear modifiers.
