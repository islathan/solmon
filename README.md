# solmon — brainSOL status display and joystick remote

A small physical display and remote control for the brainSOL tracker, on a Waveshare
ESP32-S3-AUDIO-Board in Rust (esp-hal, no_std). It keeps one WebSocket open to brainsolpi
(`ws://<tracker>:8080/ws`) and shows:

- **LCD1602 line 1:** mode and panel angles, `AUTO P  21 T  42`
- **LCD1602 line 2:** light and estimated power, `   957lx 15.2mW` (or a short status: `-> PARK`,
  `control on`, `PIN wrong`, `read only`)
- **7 on-board RGB LEDs:** light level (`lvl`, 0..1) as a bar, red → yellow
- blue LED = connecting, red LED + `brainSOL offline` = tracker unreachable

With an admin PIN it is also a **remote control** (Freenove joystick):

- **tilt the stick** → steers the panel by hand (`op:"rate"`; the tracker switches to manual by
  itself and stops when the stick is released)
- **press the stick** → next mode: auto → hold → park → auto (from manual: back to auto)

No changes to brainsolpi are needed. Protocol: `brainsolpi/brainsolpi/server.py` (`/ws`, auth),
`commands.py` (`mode`, `rate`), `engine.py:586-608` (state frame). Codec and parsing are in
`src/ws.rs`, `src/sol.rs`, `src/control.rs`.

## ⚠ Before using the PIN

The admin PIN lets solmon move the real tracker — agree on it with the team. **Wrong PINs lock out
every admin, including the web page** (5 tries, then 30 s, doubling up to 1 h — brainsolpi
`auth.py`). solmon therefore sends the PIN at most once per boot and never retries after a
refusal; reconnects use the token from the first login. Without `SOL_PIN` solmon is a read-only
display and ignores the joystick.

## Wiring (header P1)

**Go by the signal labels, not pin numbers:** the board schematic (`ESP32-S3-AUDIO-Board_1.1.pdf`,
header P1) and Waveshare's pinout image count the 2×9 header from opposite ends (pin n ↔ 19 − n).

| Part | Pin on part | Header signal | Schematic P1 # | Pinout image # |
|---|---|---|---|---|
| LCD1602 (I2C) | VDD | 5V | 1 | 18 |
| | GND | GND | 4 | 15 |
| | SCL | GPIO10 | 6 | 13 |
| | SDA | GPIO11 | 8 | 11 |
| Joystick | VCC | 3V3 | 2 | 17 |
| | GND | GND | 3 | 16 |
| | VRX | GPIO4 | 11 | 8 |
| | VRY | GPIO5 | 13 | 6 |
| | SW | GPIO6 | 15 | 4 |

Keep GPIO19/20 (USB) and EXIO0–2 (I/O expander) free. The RGB LEDs are on the board (GPIO38). Unplug USB while wiring. **Don't touch the stick while
the board starts** — its rest position is measured then; if no plausible rest position is found
the joystick is switched off (log: `no joystick`). If a direction is reversed, flip
`INVERT_PAN` / `INVERT_TILT` / `SWAP_AXES` in `src/control.rs`.

## Offline demo (no Wi-Fi, no tracker)

```sh
. ~/export-esp.sh
cargo run --release --bin demo
```

Generated data through the same display code (`src/display.rs`), 60 s cycle: no data → sunny with
the panel sweeping → a cloud → outage → sunny. With the joystick connected it shows what would be
sent (`rate +0.6 -0.2`, `-> HOLD`) and plays the tracker's part (MAN while steering, HOLD/PARK on
press) — use it to check wiring and stick directions.

## Run

All devices on the **same 2.4 GHz network** (phone hotspot; venue Wi-Fi usually blocks
device-to-device traffic). WPA2 or WPA2/WPA3 mixed — WPA3-only is not supported by esp-radio.

1. A tracker: the real Pi (port 8080), or the brainsolpi simulation on a laptop
   ([brainsolpi](https://github.com/swimmingbrain/brainsolpi)), with a test PIN:
   ```sh
   cd path/to/brainsolpi
   uv run brainsolpi run --driver sim --host 0.0.0.0 --http-port 8080 --pin 1234 \
       --config-dir /tmp/sol-cfg --data-dir /tmp/sol-data
   ```
   Its web page (`http://<laptop>:8080`, or `https://sol.swimmingbrain.dev/?pi=ws://localhost:8080`
   on the laptop) shows the 3D twin move when you steer.
   If the laptop runs ufw: `sudo ufw allow 8080/tcp` (undo with `sudo ufw delete allow 8080/tcp`).
2. Find its IP (`ip -br addr` on the laptop, or the hotspot's device list).
3. Build and flash (settings go in as environment variables, not into the code):
   ```sh
   . ~/export-esp.sh
   WIFI_SSID='name' WIFI_PASS='password' SOL_HOST=192.168.x.y SOL_PIN=1234 cargo run --release
   ```
   Optional: `SOL_PORT` (default 8080). Leave out `SOL_PIN` for a read-only display.
   mDNS names like `brainsol.local` do not resolve on the ESP32 — use the IP.
