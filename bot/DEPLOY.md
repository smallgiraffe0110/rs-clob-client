# Deploying the Polymarket Bot on Mac Mini

This guide covers running the bot on a Mac Mini and exposing the dashboard at `hunterearls.dev/polymarket`.

## Prerequisites

- Mac Mini with macOS
- SSH access to the Mac Mini (or physical access)
- Rust toolchain installed (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`)
- A Cloudflare account with `hunterearls.dev` domain managed there

## 1. Clone & Build

```bash
ssh user@macmini   # or open terminal on the Mac Mini

git clone https://github.com/smallgiraffe0110/rs-clob-client.git
cd rs-clob-client/bot
cargo build --release
```

The release binary will be at `target/release/polymarket-bot`.

## 2. Configure

Copy and edit the config:

```bash
cp config.toml config.local.toml
```

Edit `config.local.toml` with your settings. Create a `.env` file with your API credentials:

```bash
cp .env.example .env
# Edit .env with your actual keys:
#   POLYMARKET_PRIVATE_KEY=0x...
#   POLYMARKET_API_KEY=...
#   POLYMARKET_API_SECRET=...
#   POLYMARKET_API_PASSPHRASE=...
```

## 3. Run as a Background Service

### Option A: launchd (recommended — auto-restarts, survives reboots)

Create the plist file:

```bash
cat > ~/Library/LaunchAgents/com.polymarket.bot.plist << 'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.polymarket.bot</string>
    <key>ProgramArguments</key>
    <array>
        <string>/Users/YOUR_USERNAME/rs-clob-client/bot/target/release/polymarket-bot</string>
    </array>
    <key>WorkingDirectory</key>
    <string>/Users/YOUR_USERNAME/rs-clob-client/bot</string>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/Users/YOUR_USERNAME/rs-clob-client/bot/bot.log</string>
    <key>StandardErrorPath</key>
    <string>/Users/YOUR_USERNAME/rs-clob-client/bot/bot.err</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>HOME</key>
        <string>/Users/YOUR_USERNAME</string>
    </dict>
</dict>
</plist>
PLIST
```

Replace `YOUR_USERNAME` with your macOS username, then load it:

```bash
launchctl load ~/Library/LaunchAgents/com.polymarket.bot.plist
```

To check status / stop / restart:

```bash
launchctl list | grep polymarket
launchctl unload ~/Library/LaunchAgents/com.polymarket.bot.plist   # stop
launchctl load ~/Library/LaunchAgents/com.polymarket.bot.plist     # start
```

### Option B: Quick & dirty (for testing)

```bash
nohup ./target/release/polymarket-bot > bot.log 2>&1 &
```

## 4. Expose via Cloudflare Tunnel

This is the easiest way to make the dashboard accessible at `hunterearls.dev/polymarket` without port forwarding or a static IP.

### Install cloudflared

```bash
brew install cloudflared
```

### Authenticate

```bash
cloudflared tunnel login
```

This opens a browser — select the `hunterearls.dev` domain.

### Create a tunnel

```bash
cloudflared tunnel create polymarket-bot
```

Note the tunnel ID (e.g., `abc123-def456-...`).

### Configure the tunnel

Create `~/.cloudflared/config.yml`:

```yaml
tunnel: <TUNNEL_ID>
credentials-file: /Users/YOUR_USERNAME/.cloudflared/<TUNNEL_ID>.json

ingress:
  - hostname: hunterearls.dev
    path: /polymarket/*
    service: http://localhost:3030
  - hostname: hunterearls.dev
    path: /polymarket
    service: http://localhost:3030
  - service: http_status:404
```

### Add DNS route

```bash
cloudflared tunnel route dns polymarket-bot hunterearls.dev
```

If `hunterearls.dev` already has a CNAME for the root, you may need to use a subdomain like `bot.hunterearls.dev` instead:

```bash
cloudflared tunnel route dns polymarket-bot bot.hunterearls.dev
```

And update `config.yml` hostnames to `bot.hunterearls.dev`.

### Run the tunnel as a service

```bash
# Test first
cloudflared tunnel run polymarket-bot

# Install as a launchd service (auto-start on boot)
sudo cloudflared service install
```

Or create a separate launchd plist for it (similar to the bot plist above).

## 5. Verify

1. Open `https://hunterearls.dev/polymarket` (or `https://bot.hunterearls.dev`)
2. You should see the dashboard with live WebSocket connection
3. Check bot logs: `tail -f ~/rs-clob-client/bot/bot.log`

## 6. Updating

To deploy a new version:

```bash
cd ~/rs-clob-client
git pull origin main
cd bot
cargo build --release
launchctl unload ~/Library/LaunchAgents/com.polymarket.bot.plist
launchctl load ~/Library/LaunchAgents/com.polymarket.bot.plist
```

## Troubleshooting

- **Bot won't start**: Check `bot.err` for errors. Make sure `.env` exists with valid keys.
- **Dashboard loads but no data**: Check WebSocket connection in browser devtools. The WS URL must match the tunnel hostname.
- **Tunnel not working**: Run `cloudflared tunnel run polymarket-bot` manually to see errors.
- **WebSocket path issues**: The bot serves WS at `/ws`. If using a path prefix like `/polymarket`, you may need to adjust the bot's dashboard server to handle the prefix, or configure Cloudflare to strip it. Simplest approach: use a subdomain (`bot.hunterearls.dev`) so no path rewriting is needed.

## Architecture

```
Browser  --->  Cloudflare Tunnel  --->  Mac Mini (localhost:3030)
                                         |
                                         +-- polymarket-bot binary
                                              |-- Dashboard (HTTP + WebSocket)
                                              |-- Copy Tracker (polls Polymarket API)
                                              |-- Wallet Scorer (scores leaders)
                                              +-- Engine (simulates/executes trades)
```
