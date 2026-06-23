# Mac mini Test Bring-Up

This is the shortest path to getting `macrdp` running on a spare Mac mini with
stable credentials and the right macOS permissions.

## 1. Build

```bash
cd /Users/milo/code/macronrdp
cargo build --release --bin macrdp-server
```

## 2. Create a local config

Use a fixed config file instead of relying on generated credentials:

```bash
mkdir -p ~/.config/macrdp
cat > ~/.config/macrdp/config.toml <<'EOF'
port = 13389

username = "macrdp"
password = "change-this-now"

width = 0
height = 0
frame_rate = 60

quality = "high_quality"
encoder = "hardware"

log_level = "info"
idle_timeout_secs = 1800
EOF
```

Suggested first edits:
- set a real password
- keep `encoder = "hardware"` unless VideoToolbox proves flaky
- keep `port = 13389` during testing to avoid privileged-port hassle

## 3. Start it once manually

```bash
cd /Users/milo/code/macronrdp
cargo run --release --bin macrdp-server
```

Expected listen target:

```text
0.0.0.0:13389
```

## 4. Grant macOS permissions

The server will not be useful until both are granted:

- `System Settings -> Privacy & Security -> Screen Recording`
- `System Settings -> Privacy & Security -> Accessibility`

Grant them to the exact binary identity you are running.

For a release run from this repo, that is typically:

```text
/Users/milo/code/macronrdp/target/release/macrdp-server
```

Important:
- debug and release builds are different binary identities to macOS
- if you rebuild and macOS forgets trust, remove the old entry and re-add it

## 5. Connect

From an RDP client, connect to:

```text
<mac-mini-ip>:13389
```

Use the username/password from `~/.config/macrdp/config.toml`.

## 6. First test checklist

- Can you log in from an RDP client?
- Does the desktop render at all?
- Does mouse movement line up correctly?
- Does keyboard input work?
- Does resize behave sensibly?
- Does the session stay stable for a few minutes?

## 7. If capture still fails

Watch the server log:

```bash
tail -f /Users/milo/code/macronrdp/macrdp.log
```

Common causes:
- Screen Recording not granted
- Accessibility not granted
- permission granted to the wrong binary identity
- trying debug first and then testing release, or the reverse
