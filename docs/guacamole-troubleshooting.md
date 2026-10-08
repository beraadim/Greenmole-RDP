# Guacamole viewport resize troubleshooting

The 2026-10-08 test established that the Mac connection's **Resize method** was
unset, which Guacamole treats as **None**. This explains why reconnecting picked
up the browser size while resizing an existing browser window only scaled the
image. The Mac logs confirmed that Guacamole rejected Display Control channel
creation and sent no monitor layouts.

## Check the Mac connection's settings

In Guacamole administration, edit the specific RDP connection used for macrdp:

1. Set **Resize method** to **Display update** (`resize-method=display-update`).
2. Leave **Width** and **Height** unset so the initial size follows the browser.
3. Save, fully disconnect that connection, and connect again before testing.

Keep macrdp's resolution set to **Auto**. Its fixed-resolution mode intentionally
ignores client resize requests.

These are per-connection settings, so check the Mac connection even if a Windows
connection already resizes correctly. Guacamole's default resize method takes no
action on viewport changes; `reconnect` instead starts a new RDP connection.
See the [official RDP display settings](https://guacamole.apache.org/doc/gug/configuring-guacamole.html#rdp-display-settings).

## Identify where resizing stops

After changing the setting and reconnecting, resize the browser without
disconnecting. Check macrdp's logs:

| Log evidence | Meaning and next step |
| --- | --- |
| `DisplayControl (CreationFailed(3221225473))` | Client rejected channel creation with `0xC0000001`; live resize is unavailable. |
| `Display Control channel opened` | Channel creation succeeded. Resize the browser and look for a layout message. |
| `Received client Display Control layout` | The server received new dimensions. Check the following resolution and capture logs if the desktop remains unchanged. |
| `Applying resolution change` and `Activated capture and pointer desktop size` | Correlate requested, normalized, and activated sizes to diagnose the macrdp display/capture path. |

Success requires a received layout and an activated desktop size matching it
after normalization. Initial dimensions or an open graphics channel alone do
not establish live resize support.

## Only if channel creation still fails after correcting the setting

Inspect **guacd** logs for `Resize method: display-update` to verify the effective
configuration. Guacamole enables Display Control only for that method; see its
[RDP settings implementation](https://github.com/apache/guacamole-server/blob/master/src/protocols/rdp/settings.c).
If correct, record the guacd/FreeRDP versions and check for `disp` plugin-loading
errors. Guacamole's [Display Control implementation](https://github.com/apache/guacamole-server/blob/master/src/protocols/rdp/channels/disp.c)
sends layouts only after that plugin connects. A custom package may need its
matching FreeRDP channel dependencies repaired.

The failure code alone does not prove a missing plugin: FreeRDP 2.11.7 also uses
it for other channel creation failures. See the
[FreeRDP implementation](https://github.com/FreeRDP/FreeRDP/blob/2.11.7/channels/drdynvc/client/drdynvc_main.c#L950-L1047).
