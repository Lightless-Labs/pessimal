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

Copy `pessimal.example.toml` and set `endpoint` and `preset`. Do not put the API key in this file. Set
it in the `PESSIMAL_API_KEY` environment variable. The top of the example file lists the other
`PESSIMAL_*` variables.

## Linux

```sh
sudo install -m 0755 pessimal-agent /usr/local/bin/pessimal-agent
sudo mkdir -p /etc/pessimal
sudo install -m 0644 pessimal.example.toml /etc/pessimal/pessimal.toml
sudo "$EDITOR" /etc/pessimal/pessimal.toml          # set endpoint and preset

# The key goes in a file that only root can read. systemd reads it before it starts the agent.
sudo sh -c 'umask 077; printf "PESSIMAL_API_KEY=%s\n" "YOUR-KEY-HERE" > /etc/pessimal/pessimal.env'

sudo install -m 0644 pessimal-agent.service /etc/systemd/system/pessimal-agent.service
sudo systemctl daemon-reload && sudo systemctl enable --now pessimal-agent
```

See the log with `journalctl -u pessimal-agent -f`. To see the result of each export, add
`PESSIMAL_LOG=debug` to `/etc/pessimal/pessimal.env`.

The service runs as a temporary user that cannot write to the filesystem. That user must be able to read
`/etc/pessimal/pessimal.toml`, so keep that file at mode 0644.

These binaries need glibc 2.28 or later: Debian 10, Ubuntu 18.10 or RHEL 8, or newer. On an older
system the binary does not start.

## macOS

```sh
sudo install -m 0755 pessimal-agent /usr/local/bin/pessimal-agent
```

To run it as a service, use `scripts/install-agent-launchd.sh` from the repository.

The binary is signed and notarized, but macOS must ask Apple about it the first time it runs. On a host
that cannot reach Apple, the first start can fail, and under launchd you see no error. See
[GATEKEEPER.md](https://github.com/Lightless-Labs/pessimal/blob/main/packaging/macos/GATEKEEPER.md).

If you downloaded this archive in a browser and opened it in Finder, macOS marked the binary with
`com.apple.quarantine`. `cp`, `install` and `ditto` keep that mark. `curl` with `tar xzf`, mise and ubi
do not add it.

## Licence

GNU Affero General Public License v3.0. The full text is in `LICENSE`. The source code for this build is
at <https://github.com/Lightless-Labs/pessimal>, at the tag with the same version as this archive.
