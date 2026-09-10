#!/usr/bin/env python3
"""A very small App Store Connect API client.

Exists because the guests have Python 3.9 with no third-party packages, and the one hard part --
signing an ES256 JWT -- can be delegated to the `openssl` binary that every macOS carries. Everything
else the release path needs from App Store Connect is a plain GET.

    asc.py list-profiles
    asc.py install-profile --name com.lightless-labs.pessimal.ios

Reads from the environment:
    APP_STORE_CONNECT_API_KEY_ID         the key id (the 'kid' claim)
    APP_STORE_CONNECT_API_KEY_ISSUER_ID  the issuer id (the 'iss' claim)
    APP_STORE_CONNECT_API_KEY_BASE64     the .p8 private key, base64 encoded

Prints profile names, types and states. Never prints the key, the JWT, or profile contents.
"""

import argparse
import base64
import binascii
import json
import os
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

API = "https://api.appstoreconnect.apple.com/v1"


def die(message):
    print(f"error: {message}", file=sys.stderr)
    sys.exit(2)


def b64url(raw: bytes) -> str:
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode()


def der_to_raw_signature(der: bytes) -> bytes:
    """ECDSA P-256: DER SEQUENCE{INTEGER r, INTEGER s} -> the 64-byte r||s JOSE wants."""
    if not der or der[0] != 0x30:
        die("openssl did not return a DER SEQUENCE; cannot build the JWT signature")
    # Skip SEQUENCE tag and its length (short or long form).
    index = 2 if der[1] < 0x80 else 2 + (der[1] & 0x7F)

    def read_integer(pos):
        if der[pos] != 0x02:
            die("malformed DER signature: expected an INTEGER")
        length = der[pos + 1]
        value = der[pos + 2 : pos + 2 + length]
        # DER integers are signed, so a leading zero may pad a high bit. Strip it, then left-pad to 32.
        return value.lstrip(b"\x00").rjust(32, b"\x00"), pos + 2 + length

    r, index = read_integer(index)
    s, _ = read_integer(index)
    return r + s


def jwt() -> str:
    key_id = os.environ.get("APP_STORE_CONNECT_API_KEY_ID") or die("APP_STORE_CONNECT_API_KEY_ID is not set")
    issuer = os.environ.get("APP_STORE_CONNECT_API_KEY_ISSUER_ID") or die("APP_STORE_CONNECT_API_KEY_ISSUER_ID is not set")
    key_b64 = os.environ.get("APP_STORE_CONNECT_API_KEY_BASE64") or die("APP_STORE_CONNECT_API_KEY_BASE64 is not set")

    try:
        pem = base64.b64decode(key_b64, validate=True)
    except (binascii.Error, ValueError):
        die("APP_STORE_CONNECT_API_KEY_BASE64 is not valid base64")
    if b"PRIVATE KEY" not in pem:
        die("APP_STORE_CONNECT_API_KEY_BASE64 does not decode to a PEM private key")

    now = int(time.time())
    header = {"alg": "ES256", "kid": key_id, "typ": "JWT"}
    # Apple rejects anything longer than 20 minutes.
    claims = {"iss": issuer, "iat": now, "exp": now + 1140, "aud": "appstoreconnect-v1"}
    signing_input = f"{b64url(json.dumps(header, separators=(',', ':')).encode())}." \
                    f"{b64url(json.dumps(claims, separators=(',', ':')).encode())}"

    # The key is written 0600 into a directory only this process can read, and removed immediately.
    with tempfile.TemporaryDirectory() as workdir:
        key_path = os.path.join(workdir, "key.p8")
        with open(os.open(key_path, os.O_WRONLY | os.O_CREAT, 0o600), "wb") as handle:
            handle.write(pem)
        result = subprocess.run(
            ["openssl", "dgst", "-sha256", "-sign", key_path],
            input=signing_input.encode(), capture_output=True,
        )
    if result.returncode != 0:
        die(f"openssl could not sign the JWT: {result.stderr.decode(errors='replace').strip()}")
    return f"{signing_input}.{b64url(der_to_raw_signature(result.stdout))}"


def get(path, params=None):
    url = f"{API}/{path}"
    if params:
        url += "?" + urllib.parse.urlencode(params)
    request = urllib.request.Request(url, headers={
        "Authorization": f"Bearer {jwt()}",
        "Accept": "application/json",
        "User-Agent": "pessimal-release/1",
    })
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.load(response)
    except urllib.error.HTTPError as exc:
        body = exc.read(600).decode(errors="replace")
        if exc.code == 401:
            die("App Store Connect rejected the key (401). Check the key id, issuer id and that the "
                f"key has not been revoked. Body: {body}")
        die(f"App Store Connect returned HTTP {exc.code} for {path}: {body}")
    except Exception as exc:  # noqa: BLE001 - the message matters more than the class
        die(f"App Store Connect request for {path} failed: {exc}")


def all_profiles():
    profiles, params = [], {"limit": 200, "include": "bundleId"}
    page = get("profiles", params)
    bundle_ids = {item["id"]: item["attributes"].get("identifier")
                  for item in page.get("included", []) if item.get("type") == "bundleIds"}
    for entry in page.get("data", []):
        attributes = entry.get("attributes", {})
        related = (entry.get("relationships", {}).get("bundleId", {}).get("data") or {}).get("id")
        profiles.append({
            "id": entry["id"],
            "name": attributes.get("name"),
            "type": attributes.get("profileType"),
            "state": attributes.get("profileState"),
            "expires": attributes.get("expirationDate"),
            "bundle_id": bundle_ids.get(related),
        })
    return profiles


def command_list(_args):
    profiles = all_profiles()
    if not profiles:
        print("the key can see no provisioning profiles at all")
        return
    print(f"{len(profiles)} profile(s) visible to this key:")
    for profile in sorted(profiles, key=lambda p: (p["bundle_id"] or "", p["name"] or "")):
        print(f"  name={profile['name']!r} type={profile['type']} state={profile['state']} "
              f"bundle={profile['bundle_id']!r} expires={profile['expires']}")


def command_install(args):
    profiles = all_profiles()
    matches = [p for p in profiles if p["name"] == args.name]
    if args.type:
        matches = [p for p in matches if p["type"] == args.type]
    if not matches:
        print(f"no profile named {args.name!r}"
              + (f" of type {args.type}" if args.type else "") + ". Visible profiles:", file=sys.stderr)
        for profile in profiles:
            print(f"  name={profile['name']!r} type={profile['type']} state={profile['state']} "
                  f"bundle={profile['bundle_id']!r}", file=sys.stderr)
        sys.exit(2)

    active = [p for p in matches if p["state"] == "ACTIVE"] or matches
    chosen = active[0]
    if chosen["state"] != "ACTIVE":
        die(f"the only profile named {args.name!r} is {chosen['state']}, not ACTIVE")

    # The list endpoint omits profileContent, so fetch the one profile.
    content = get(f"profiles/{chosen['id']}")["data"]["attributes"].get("profileContent")
    if not content:
        die("App Store Connect returned the profile without its content")

    target_dir = os.path.expanduser("~/Library/MobileDevice/Provisioning Profiles")
    os.makedirs(target_dir, exist_ok=True)
    # Bazel's local_provisioning_profile matches the Name *inside* the profile, so the filename is
    # free; the profile id keeps repeated runs from piling up near-duplicates.
    target = os.path.join(target_dir, f"{chosen['id']}.mobileprovision")
    with open(target, "wb") as handle:
        handle.write(base64.b64decode(content))
    print(f"installed {chosen['name']!r} ({chosen['type']}, expires {chosen['expires']}) at {target}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("list-profiles", help="print every profile the key can see").set_defaults(func=command_list)
    install = sub.add_parser("install-profile", help="download one profile into ~/Library/MobileDevice")
    install.add_argument("--name", required=True)
    install.add_argument("--type", default="IOS_APP_STORE")
    install.set_defaults(func=command_install)
    args = parser.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
