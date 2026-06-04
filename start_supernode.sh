#!/usr/bin/env bash
# Start Conquerd as a supernode (headless TURN relay)
#
#   supernode_invite_ttl  — invite lifetime in minutes (-1 = never expires)
#   supernode_port        — TURN relay UDP port (default 3478)
#   supernode_host        — public IP or hostname for relay tickets (required for remote clients)
#   supernode_signaling_port — WebSocket signaling port (default auto)
#   supernode_chat        — enable chat relay (1=on, 0=off, default 1)
#   supernode_files       — enable file transfer (1=on, 0=off, default 1)
#   supernode_updates     — enable P2P auto-updates (1=on, 0=off, default 1)
#   supernode_auto_restart — auto-restart after update applied (1=on, 0=off, default 1)
#   supernode_web_port    — HTTPS port for the node homepage/portal (unset = disabled)
#   supernode_web_title   — human-readable name shown on the homepage (default "Relay Node")
#   supernode_access_mode — portal access mode: open|tos|ad|code (default "open")
#   supernode_access_code — access code for 'code' mode (default "conquerd")
#   supernode_ad_duration — countdown seconds for 'ad' mode (default 30)
#   supernode_tos_text    — custom TOS text for 'tos' mode (or edit portal/tos.html)
#   supernode_ad_content  — HTML content for the ad/timer waiting area
#
# Voice is never used — supernodes are headless relays.
# The invite link will be printed to the console. Share it with peers.

export CONQUERD_HOME="$HOME/.conquerd"
export supernode=1
export supernode_invite_ttl=-1
export supernode_port=3478
export supernode_host=
export supernode_signaling_port=34935
export supernode_web_port=8443
export supernode_web_title="My Relay Node"

# --- Portal access mode (uncomment ONE block) ---

# Mode: open — no gate, all peers granted immediately
#export supernode_access_mode=open

# Mode: tos — require Terms of Service acceptance
export supernode_access_mode=tos
export supernode_tos_text="By using this relay you agree to behave."

# Mode: ad — show content/ad with a countdown timer before granting access
#export supernode_access_mode=ad
#export supernode_ad_duration=15
#export supernode_ad_content='<p style="color:#7ecfff;">Thank you for supporting this relay node!</p>'

# Mode: code — require an access code to connect
#export supernode_access_mode=code
#export supernode_access_code=changeme

# --- Demo mode (shows nav links to all portal pages regardless of active mode) ---
export supernode_demo_links=1

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
"$SCRIPT_DIR/rust/target/release/conquerd-supernode"
