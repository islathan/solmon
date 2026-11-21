# solmon — brainSOL status display

A small physical display for the brainSOL tracker, on a Waveshare ESP32-S3-AUDIO-Board in Rust
(esp-hal, no_std). Every second it fetches `GET /api/history?span=2` from brainsolpi over Wi-Fi
and shows:

- **LCD1602 line 1:** panel angles, `Pan  21 Tilt  42`
- **LCD1602 line 2:** light and estimated power, `   957lx 15.2mW`
- **7 on-board RGB LEDs:** light level (`lvl`, 0..1) as a bar, red → yellow
- Status: blue LED = connecting / no light value yet, red LED + `brainSOL offline` = tracker unreachable

No changes to brainsolpi are needed. Data format: `brainsolpi/brainsolpi/history.py:13`,
field meaning: `brainsolpi/brainsolpi/engine.py:586-604`.

## Wiring

LCD1602 (I2C) on header P1: VDD → pin 1 (5V), GND → pin 4, SCL → pin 6, SDA → pin 8.
Pin numbers from the board schematic (Waveshare `ESP32-S3-AUDIO-Board_1.1.pdf`, header P1).
The RGB LEDs are on the board (GPIO38), no wiring.

## Offline demo (no Wi-Fi, no tracker)

```sh
. ~/export-esp.sh
cargo run --release --bin demo
```

Generates test data on the board and runs it through the same display code (`src/display.rs`)
as the real monitor, in a 60 s cycle: no data → sunny with the panel sweeping → a cloud passes →
outage ("brainSOL offline", red LED) → sunny. Same cycle as `tools/fake_tracker.py`.

## Run

All devices on the **same 2.4 GHz network** (phone hotspot; venue Wi-Fi usually blocks
device-to-device traffic). WPA2 or WPA2/WPA3 mixed — WPA3-only is not supported by esp-radio.

1. Something that serves the tracker API:
   - the real Pi (port 8080), or
   - the fake tracker (no brainsolpi needed, runs through every display state in 60 s):
     ```sh
     python3 tools/fake_tracker.py            # 0.0.0.0:8080; --phase sunny|cloud|empty|down to hold one state
     ```
   - the simulation on a laptop:
     ```sh
     cd path/to/brainsolpi   # https://github.com/swimmingbrain/brainsolpi
     uv run brainsolpi run --driver sim --host 0.0.0.0 --http-port 8080 \
         --config-dir /tmp/sol-cfg --data-dir /tmp/sol-data
     ```
     If the laptop runs ufw: `sudo ufw allow 8080/tcp` (undo with `sudo ufw delete allow 8080/tcp`).
2. Find its IP (`ip -br addr` on the laptop, or the hotspot's device list).
3. Build and flash (settings go in as environment variables, not into the code):
   ```sh
   . ~/export-esp.sh
   WIFI_SSID='name' WIFI_PASS='password' SOL_HOST=192.168.x.y cargo run --release
   ```
   Optional: `SOL_PORT=8080`. mDNS names like `brainsol.local` do not resolve on the ESP32 — use the IP.
