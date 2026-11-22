# Infrastructure setup

Guide can help set up machines, a VPS, remote access, and Computer. Every setting change creates a review card. Select **Apply** only after checking its target and settings.

## Machines

Ask for the new machine's name and exact HTTPS origin. Both machines need addresses they can reach. Use `add_guide_machine` to propose setup. After Apply, the card gives the user a private install command to run on the new machine and checks for pairing and health. Use `list_guide_machines` to check the same origin later. Open [[settings.machines]] for manual pairing.

## VPS over SSH

Use a fresh Linux user with user systemd and an HTTPS proxy to the Minarbot port. In [[settings.vault]], add a **Secret** with the SSH password and `https://<ssh-host>` as the site URL. Grant Minarbot Guide access. Ask for the Vault entry name, SSH host and user, VPS HTTPS origin, and the host's Ed25519 public key (type and base64 value) from the VPS console. Use `install_guide_vps` to propose setup.

Apply runs the signed installer, pairs the VPS, and checks health. If setup fails, follow the card's recovery steps. Check the VPS and [[settings.machines]] before retrying.

## Remote access

Use `inspect_guide_remote_access` to check Tailscale sign-in, MagicDNS, and HTTPS Serve. Use `configure_guide_remote_access` with the detected tailnet-only HTTPS origin. After Apply, restart Minarbot and check reachability again. Open [[settings.devices]] from the card. On the phone or other machine, join the same tailnet, scan the QR, and enter the pairing code. A successful pairing confirms the device can reach Minarbot.

## Computer

Use `read_guide_computer_setup` to check readiness and `setup_guide_computer` to propose setup. After Apply, open [[settings.computer]] to follow progress. If setup stops, explain the missing programs or permissions reported there.
