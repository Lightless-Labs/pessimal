# Pessimal agent

The agent reads CPU, memory, disk and network metrics from this host and sends them over OTLP to the
endpoint you configure. It needs no root access. It sends nothing until you give it an endpoint.

```
pessimal-agent            the program
pessimal.example.toml     an example config
pessimal-agent.service    a systemd unit (Linux only)
LICENSE                   AGPL-3.0
README.md                 this file
```

## Check the download

Download `SHA256SUMS` from the same release as this archive, then:

```sh
shasum -a 256 -c --ignore-missing SHA256SUMS
```

Look for `OK` next to the name of your archive. `sha256sum` takes the same arguments.

## Configure

```sh
pessimal-agent init
```

It asks where to send metrics, which environment this host belongs to, and for the API key, which it
does not echo. It writes the config where the service on this machine reads it, sends one batch to
check the backend accepts it, and offers to start the service. Run it again to change an answer; the
previous config is kept as `pessimal.toml.bak`.

`pessimal.example.toml` lists every field and every `PESSIMAL_*` environment variable, for a config
you would rather write yourself.

## Linux

```sh
sudo install -m 0755 pessimal-agent /usr/local/bin/pessimal-agent
sudo install -m 0644 pessimal-agent.service /etc/systemd/system/pessimal-agent.service
sudo systemctl daemon-reload

sudo pessimal-agent init
```

As root, `init` writes `/etc/pessimal/pessimal.toml` at mode 0644 and puts the key in
`/etc/pessimal/pessimal.env` at mode 0600. The two files are split because the unit runs the agent
under `DynamicUser=yes`: that transient account has to read the config, so the key cannot be in it.
systemd reads the environment file as root. `init` then offers to run
`systemctl enable --now pessimal-agent`.

See the log with `journalctl -u pessimal-agent -f`. To see the result of each export, add
`PESSIMAL_LOG=debug` to `/etc/pessimal/pessimal.env`.

The service runs as a temporary user that cannot write to the filesystem. That user must be able to read
`/etc/pessimal/pessimal.toml`, so keep that file at mode 0644 and the key out of it.

These binaries need glibc 2.28 or later: Debian 10, Ubuntu 18.10 or RHEL 8, or newer. On an older
system the binary does not start.

## macOS

```sh
sudo install -m 0755 pessimal-agent /usr/local/bin/pessimal-agent
pessimal-agent init
```

On macOS the key goes in the config file, which `init` writes at mode 0600 under
`~/.config/pessimal/`. To run it as a service, use `scripts/install-agent-launchd.sh` from the
repository; `init` notices that LaunchAgent if it is already loaded, and does not suggest a second
one.

The binary is signed and notarized, but macOS must ask Apple about it the first time it runs. On a host
that cannot reach Apple, the first start can fail, and under launchd you see no error. See
[GATEKEEPER.md](https://github.com/Lightless-Labs/pessimal/blob/main/packaging/macos/GATEKEEPER.md).

If you downloaded this archive in a browser and opened it in Finder, macOS marked the binary with
`com.apple.quarantine`. `cp`, `install` and `ditto` keep that mark. `curl` with `tar xzf`, mise and ubi
do not add it.

## Licence

GNU Affero General Public License v3.0. The full text is in `LICENSE`. The source code for this build is
at <https://github.com/Lightless-Labs/pessimal>, at the tag with the same version as this archive.
