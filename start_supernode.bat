@echo off
REM Start Conquerd as a supernode (headless TURN relay)
REM
REM   supernode_invite_ttl  — invite lifetime in minutes (-1 = never expires)
REM   supernode_port        — TURN relay UDP port (default 3478)
REM   supernode_host        — public IP or hostname for relay tickets (required for remote clients)
REM   supernode_signaling_port — WebSocket signaling port (default auto)
REM   supernode_chat        — enable chat relay (1=on, 0=off, default 1)
REM   supernode_files       — enable file transfer (1=on, 0=off, default 1)
REM   supernode_updates     — enable P2P auto-updates (1=on, 0=off, default 1)
REM   supernode_auto_restart — auto-restart after update applied (1=on, 0=off, default 1)
REM   supernode_web_port    — HTTPS port for the node homepage/portal (unset = disabled)
REM   supernode_web_title   — human-readable name shown on the homepage (default "Relay Node")
REM   supernode_access_mode — portal access mode: open|tos|ad|code (default "open")
REM   supernode_access_code — access code for 'code' mode (default "conquerd")
REM   supernode_ad_duration — countdown seconds for 'ad' mode (default 30)
REM   supernode_tos_text    — custom TOS text for 'tos' mode (or edit portal/tos.html)
REM   supernode_ad_content  — HTML content for the ad/timer waiting area
REM
REM Voice is never used — supernodes are headless relays.
REM The invite link will be printed to the console. Share it with peers.

set CONQUERD_HOME=%USERPROFILE%\.conquerd
set supernode=1
set supernode_invite_ttl=-1
set supernode_port=3478
set supernode_host=
set supernode_signaling_port=34935
set supernode_web_port=8443
set supernode_web_title=My Relay Node

REM --- Portal access mode (uncomment ONE block) ---

REM Mode: open — no gate, all peers granted immediately
REM set supernode_access_mode=open

REM Mode: tos — require Terms of Service acceptance
set supernode_access_mode=tos
set supernode_tos_text=By using this relay you agree to behave.

REM Mode: ad — show content/ad with a countdown timer before granting access
REM set supernode_access_mode=ad
REM set supernode_ad_duration=15
REM set supernode_ad_content=<p style="color:#7ecfff;">Thank you for supporting this relay node!</p>

REM Mode: code — require an access code to connect
REM set supernode_access_mode=code
REM set supernode_access_code=changeme

REM --- Demo mode (shows nav links to all portal pages regardless of active mode) ---
set supernode_demo_links=1

"%~dp0rust\target\release\conquerd-supernode.exe"
