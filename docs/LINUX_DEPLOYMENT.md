# Kageha on a Linux server

The Linux x64 bundle can run in the foreground with `./start` or as a single-owner service. “This host” in the chat picker refers to the server running Kageha. SSH Remote selects a separate Linux host. Docker Workstation runs an isolated desktop in a container on the Kageha server.

## Install the service

Create a dedicated `kageha` system user and group with home `/var/lib/kageha`. Extract the matching release under `/opt/kageha/releases/<version>` and make `/opt/kageha/current` point to that directory. Keep the bundle and service unit owned by the administrator. Install `share/kageha/linux/kageha.service` under `/etc/systemd/system/` and run `sudo systemctl daemon-reload`, then `sudo systemctl enable --now kageha`.

The launcher binds the WebUI to `127.0.0.1:8788`. Put `share/kageha/linux/kageha.nginx.conf` in nginx's `http` configuration, substitute the hostname and certificate paths, and run `sudo nginx -t` before reloading the proxy. Configure your model in Settings after signing in. Retrieve the initial sign-in token on the server with `sudo -u kageha env KAGEHA_DATA_DIR=/var/lib/kageha /opt/kageha/current/start token`. Keep the backend port and browser services private.

## Connect an SSH destination

Give the service account access to its own SSH client key or configured SSH agent. The identity path in Settings refers to a file on the Kageha server. Obtain the SSH server's **host public key** from its administrator and enter it in Settings → Remote Hosts. The connection test shows the fingerprint, host OS, and workspace access. Existing verified host entries can continue using the instance's `state/ssh_known_hosts` file. Edit a trust pin only after deactivating that host in its chats and verifying the replacement key with the administrator.

Use SSH Remote for shell work in the configured persistent workspace. The agent can verify files in place or retrieve them as chat artifacts. Download a file from the chat when you need a copy outside the SSH workspace.

## Enable a Linux browser or desktop

Use a dedicated unprivileged Linux account. Install a Chromium-compatible browser with its namespace sandbox, `curl`, and `socat`. For the full desktop, also install `xvfb`, `xauth`, `openbox`, `xdotool`, `scrot`, `xterm`, and `mcookie` from `util-linux`. Enable the account's systemd user manager with `sudo loginctl enable-linger <account>`. Run `kageha-linux-session probe` as that account to confirm readiness.

- **On the Kageha host**, set `KAGEHA_LINUX_BROWSER=headless` or `KAGEHA_LINUX_BROWSER=desktop` in `/etc/kageha/kageha.env`. The release launcher finds its bundled helper. Set `KAGEHA_LINUX_CHROMIUM` to the browser executable if it is named something other than `chromium`, then restart Kageha.
- **On a separate SSH host**, install `bin/kageha-linux-session` from the release to `/usr/local/bin/kageha-linux-session` as an administrator. If the browser needs an override, set `KAGEHA_LINUX_CHROMIUM` in the dedicated account's SSH environment. Enable Remote browser, Remote desktop, or Background jobs in the saved host settings and run Test connection again. Desktop starts its own X11 display and Chromium profile. Background jobs remain available on headless servers.

Start the selected Linux desktop from the Computer panel or let the agent start it by opening the browser or observing the display. The session helper relays Chromium and desktop data without opening a public listener. Chrome follows the ordinary download path under the selected workspace's `outputs` folder. Managed SSH jobs can be queried after Kageha or the WebUI disconnects. A host reboot interrupts jobs still in progress.

## Enable Docker Workstation

The service user needs access to the local Docker daemon. Access to that daemon is an administrative privilege on the Kageha server. Load the release workstation image with `docker load -i kageha-desktop-linux-x64.tar.gz`, or build it from the matching checkout with `docker build -f docker/desktop/Dockerfile -t kageha-desktop:machine-v3 .`. Run `/machine doctor` in Kageha to check the daemon and image. The workstation uses Linux namespaces for Chromium and needs an engine and kernel that allow its Chromium sandbox to start. `/machine status` shows the current workstation state. For an older chat workstation, select **Upgrade Linux desktop** from the execution menu. The prior container remains available as a stopped backup.

## Diagnose, upgrade, and restore

Check `systemctl status kageha` and `journalctl -u kageha` from the server. The health endpoint requires the same token as the WebUI.

```sh
token=$(sudo -u kageha env KAGEHA_DATA_DIR=/var/lib/kageha /opt/kageha/current/start token)
printf 'Authorization: Bearer %s\n' "$token" |
  curl --fail --silent --show-error --header @- http://127.0.0.1:8788/health
unset token
```

For an upgrade, stop Kageha and back up `/var/lib/kageha` in full, including the vault key, SSH pins, and chat workspaces. Back up any external SSH workspaces separately. Point `/opt/kageha/current` at the new bundle, start the service, and repeat the route checks. To roll back, stop the service, restore the pre-upgrade backup and bundle symlink, then start the service again.

## Qualify a release

Push a `release/` branch or run **Build installable Kageha** in GitHub Actions to build and test candidate archives. The workflow tests the Linux archive through nginx and systemd and runs the Docker suite against the packaged binaries. A `v*` tag also runs live Linux route qualification before drafting a release. To run that qualification manually, select **Qualify Linux** when starting the workflow.

Configure the `linux-qualification` GitHub environment with a dedicated, reachable Ubuntu x64 SSH fixture, its trusted Ed25519 host public key, a writable test workspace, and an approved live model endpoint. Install the candidate's Linux helper and GUI prerequisites on the fixture. Keep the client key and model key in environment secrets. The job records which routes passed for the candidate build.
