# Using the dissector

The dissector can be used with `wireshark` and `tshark`. It also supports **exporting replay streams** (SD card playback / download) from a pcap to a file.

## Exporting replay video from a pcap

1. Open your pcap in Wireshark (with the Baichuan dissector installed). Replay packets (message IDs 5, 8, 381) are collected automatically during dissection.
2. **Tools → Export Baichuan Replay to File** — reassembles streams (skips 32-byte metadata, concatenates until status 300/331) and writes `replay_1.bin`, `replay_2.bin`, … (or `.mp4` if the payload starts with `ftyp`).
3. Set **Edit → Preferences → Protocols → Baichuan → Replay export directory** to choose where files are saved (empty = current directory).

Exported files are raw replay payloads (often MP4). If the capture is decrypted (decryption key set), the payloads are the same as the camera sends; you can rename to `.mp4` and play, or run Neolink’s `assemble_mp4_with_mdat` logic if needed.

---

For **AES decryption** of Baichuan payloads the dissector uses, in order:

1. **Wireshark's built-in Gcrypt Lua API** (Wireshark 4.x) – no extra install. The dissector uses this when available.
2. **Optional fallback: [`luagcrypt`](https://github.com/Lekensteyn/luagcrypt)** – an older, unmaintained C module; only needed if your Wireshark build does not expose GcryptCipher to Lua. Not packaged in most Linux distributions; build from source. Instructions below are for Debian and its derivatives (tested on Debian 12 Bookworm amd64).

Without either, the dissector still decodes BC headers and XOR-encrypted XML; only AES-decrypted payloads are skipped.

## Build `luagcrypt.so` (optional fallback)
```
sudo apt install lua5.2 liblua5.2-dev libgcrypt20-dev libgpg-error-dev
git clone https://github.com/Lekensteyn/luagcrypt.git
cd luagcrypt
make LUA_DIR=/usr
```
## Install `luagcrypt.so`
The shared object library should be copied to `/usr/local/lib/lua/5.2/`
```
mkdir --parents /usr/local/lib/lua/5.2/
cp luagcrypt.so /usr/local/lib/lua/5.2/
```
Additionally, the system where the dissector is used needs these packages installing (if not already present): `libgcrypt20 libgpg-error0`
## Install `baichuan.lua`
Copy the dissector to the host where `wireshark` (or `tshark`) will be used to analyse the captured packets:
```
mkdir --parents $HOME/.local/lib/wireshark/plugins/
cp baichuan.lua $HOME/.local/lib/wireshark/plugins/
```

