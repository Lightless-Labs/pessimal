#!/usr/bin/env python3
"""Compare the codesigning identities available here with the certificates a profile will accept.

    signing-diagnostics.py <profile.mobileprovision> [keychain]

rules_apple fails signing with

    ERROR: Unable to find an identity on the system matching the ones in ...mobileprovision

which is true of three different situations and distinguishes none of them: no identity is visible at
all; an identity is visible but the profile was built around a different certificate; or the identity
is visible to this shell and not to the signing tool. This prints both sides and their intersection, so
the next step is never a guess.

Exits nonzero when the intersection is empty.
"""

import plistlib
import re
import subprocess
import sys


def run(*command, stdin=None):
    return subprocess.run(command, capture_output=True, input=stdin)


def profile_certificates(profile_path):
    """The certificates the profile accepts: (common name, SHA-1 fingerprint) for each."""
    decoded = run("security", "cms", "-D", "-i", profile_path)
    if decoded.returncode != 0:
        print(f"could not decode {profile_path}: {decoded.stderr.decode(errors='replace').strip()}",
              file=sys.stderr)
        sys.exit(2)
    try:
        plist = plistlib.loads(decoded.stdout)
    except Exception as exc:  # noqa: BLE001
        print(f"{profile_path} did not parse as a plist: {exc}", file=sys.stderr)
        sys.exit(2)

    print(f"profile: {plist.get('Name')!r}")
    print(f"  team        : {','.join(plist.get('TeamIdentifier') or [])}")
    print(f"  app id      : {(plist.get('Entitlements') or {}).get('application-identifier')}")
    print(f"  expires     : {plist.get('ExpirationDate')}")

    certificates = []
    for der in plist.get("DeveloperCertificates") or []:
        subject = run("openssl", "x509", "-inform", "DER", "-noout", "-subject", stdin=der)
        digest = run("openssl", "x509", "-inform", "DER", "-noout", "-fingerprint", "-sha1", stdin=der)
        common_name = re.search(r"CN\s*=\s*([^,/\n]+)", subject.stdout.decode(errors="replace"))
        fingerprint = re.search(r"=\s*([0-9A-Fa-f:]+)", digest.stdout.decode(errors="replace"))
        certificates.append((
            (common_name.group(1).strip() if common_name else "?"),
            (fingerprint.group(1).replace(":", "").upper() if fingerprint else "?"),
        ))
    return certificates


def visible_identities(keychain=None):
    """(SHA-1, name) for every codesigning identity the `security` tool can see."""
    command = ["security", "find-identity", "-v", "-p", "codesigning"]
    if keychain:
        command.append(keychain)
    result = run(*command)
    text = result.stdout.decode(errors="replace")
    return [(m.group(1).upper(), m.group(2)) for m in re.finditer(r'\)\s+([0-9A-F]{40})\s+"([^"]*)"', text)], text


def main():
    if len(sys.argv) < 2:
        print(__doc__, file=sys.stderr)
        sys.exit(2)
    profile_path, keychain = sys.argv[1], (sys.argv[2] if len(sys.argv) > 2 else None)

    accepted = profile_certificates(profile_path)
    print(f"  accepts {len(accepted)} certificate(s):")
    for name, fingerprint in accepted:
        print(f"    {fingerprint}  {name!r}")

    print()
    print("keychain search list:")
    for line in run("security", "list-keychains", "-d", "user").stdout.decode(errors="replace").splitlines():
        print(f"  {line.strip()}")

    for label, target in (("default search list", None), (f"keychain {keychain}", keychain)):
        if target is None and keychain is not None:
            pass  # still worth printing both
        if label.startswith("keychain") and keychain is None:
            continue
        identities, raw = visible_identities(target)
        print(f"\nidentities visible via {label}: {len(identities)}")
        for fingerprint, name in identities:
            print(f"  {fingerprint}  {name!r}")
        if not identities:
            print("  (none)  raw output:")
            for line in raw.splitlines():
                print(f"    {line}")

    everything, _ = visible_identities()
    if keychain:
        scoped, _ = visible_identities(keychain)
        everything = list({*everything, *scoped})

    accepted_fingerprints = {fingerprint for _, fingerprint in accepted}
    matching = [(f, n) for f, n in everything if f in accepted_fingerprints]

    print()
    if matching:
        print(f"MATCH: {len(matching)} identity/identities satisfy this profile:")
        for fingerprint, name in matching:
            print(f"  {fingerprint}  {name!r}")
        return

    print("NO MATCH. The profile accepts certificates that no visible identity provides.")
    print("Either the certificate in the vault is not the one this profile was built around --")
    print("regenerate the profile against the certificate being imported -- or the identity is")
    print("present without a usable private key, which `find-identity -v` would have hidden.")
    sys.exit(1)


if __name__ == "__main__":
    main()
