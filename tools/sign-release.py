"""Signs FilePilot release files for the built-in updater.

The signature is in minisign's format (Ed25519 over the file's BLAKE2b-512 hash, plus a
signed "trusted comment"), which the app checks with the minisign-verify crate against the
public key in crates/update/src/lib.rs. The trusted comment names the file and the version,
so an older signed installer cannot be passed off as a newer one.

    python tools/sign-release.py sign FILE VERSION   writes FILE.minisig
    python tools/sign-release.py public              prints the public key
    python tools/sign-release.py keygen              prints a new secret and its public key

The secret comes from the environment variable FILEPILOT_SIGNING_KEY (a GitHub Actions
secret): base64 of the 8-byte key ID followed by the 32-byte Ed25519 seed.

Needs the "cryptography" package (pip install cryptography).
"""

import base64
import hashlib
import os
import secrets
import sys
import time

from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey


def b64(data):
    return base64.b64encode(data).decode("ascii")


def load_secret():
    text = os.environ.get("FILEPILOT_SIGNING_KEY", "").strip()
    if not text:
        sys.exit("FILEPILOT_SIGNING_KEY is not set")
    raw = base64.b64decode(text, validate=True)
    if len(raw) != 40:
        sys.exit("FILEPILOT_SIGNING_KEY must hold 40 bytes (key ID + seed)")
    return raw[:8], Ed25519PrivateKey.from_private_bytes(raw[8:])


def public_key(key_id, key):
    raw = key.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)
    return b64(b"Ed" + key_id + raw)


def sign(path, version, key_id, key):
    with open(path, "rb") as f:
        data = f.read()
    name = os.path.basename(path)
    signature = key.sign(hashlib.blake2b(data, digest_size=64).digest())
    trusted = f"file:{name}\tversion:{version}\ttimestamp:{int(time.time())}"
    global_signature = key.sign(signature + trusted.encode("utf-8"))
    text = (
        "untrusted comment: FilePilot release signature\n"
        + b64(b"ED" + key_id + signature) + "\n"
        + "trusted comment: " + trusted + "\n"
        + b64(global_signature) + "\n"
    )
    with open(path + ".minisig", "w", encoding="ascii", newline="\n") as f:
        f.write(text)
    print(f"signed {name} ({version})")


def main(args):
    if args[:1] == ["sign"] and len(args) == 3:
        key_id, key = load_secret()
        sign(args[1], args[2], key_id, key)
    elif args == ["public"]:
        print(public_key(*load_secret()))
    elif args == ["keygen"]:
        key_id, seed = secrets.token_bytes(8), secrets.token_bytes(32)
        print("secret (FILEPILOT_SIGNING_KEY):", b64(key_id + seed))
        print("public key:", public_key(key_id, Ed25519PrivateKey.from_private_bytes(seed)))
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv[1:])
