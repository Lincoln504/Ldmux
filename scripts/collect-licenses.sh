#!/usr/bin/env bash
# Gather the licence texts that must travel with a distributed binary.
#
# MIT, BSD-3-Clause and Apache-2.0 all require the notice to accompany the artifact, not to live
# in a repository the user never sees. `THIRD-PARTY-NOTICES.md` names what is used; this copies the
# actual texts, one directory per package, so the archive carries them.
#
# Output: dist/licenses/ (crates/, vendor/, LICENSE, THIRD-PARTY-NOTICES.md).
set -euo pipefail
cd "$(dirname "$0")/.."

out=dist/licenses
rm -rf "$out"
mkdir -p "$out/crates"

# Filtered to the platform that ships: a crate resolved only for wasm or UEFI is not in the
# binary, so its notice is not owed by this artifact.
meta=$(mktemp "${TMPDIR:-/tmp}/ldmux-meta.XXXXXX.json")
trap 'rm -f "$meta"' EXIT
cargo metadata --format-version 1 --locked \
    --filter-platform "$(rustc -vV | awk '/^host: /{print $2}')" > "$meta"
python3 - "$out" "$meta" <<'PY'
import json, os, pathlib, shutil, sys

out = pathlib.Path(sys.argv[1])
meta = json.load(open(sys.argv[2]))
registry = pathlib.Path(os.path.expanduser("~/.cargo/registry/src"))

copied = 0
missing = []
for p in meta["packages"]:
    if not p.get("source"):
        continue
    name, version = p["name"], p["version"]
    dest = out / "crates" / f"{name}-{version}"
    dest.mkdir(parents=True, exist_ok=True)
    # A registry crate unpacks to <name>-<version> under a registry hash directory.
    candidates = list(registry.glob(f"*/{name}-{version}"))
    if not candidates:
        missing.append(f"{name}-{version}")
        continue
    src = candidates[0]
    # Some crates (r-efi) keep their texts in a LICENSES/ directory rather than beside the source.
    found = [f for f in src.iterdir()
             if f.name.upper().startswith(("LICENSE", "COPYING", "NOTICE", "UNLICENSE"))]
    if not found:
        missing.append(f"{name}-{version}")
        continue
    for f in found:
        if f.is_dir():
            shutil.copytree(f, dest / f.name, dirs_exist_ok=True)
        else:
            shutil.copy2(f, dest / f.name)
    copied += 1

print(f"crates: {copied} copied, {len(missing)} without a licence file")
for m in missing[:10]:
    print(f"  no licence file in the unpacked source: {m}")
# **A missing text is a failure, not a note.** The artifact owes the notice, and a count that
# nobody reads is how a bundle ships without it. The one legitimate reason for a crate to be
# unmatched is that the registry is not unpacked on this machine (a fresh CI runner), which is
# also a build that cannot collect: say so and fail.
if missing:
    sys.stderr.write(
        f"collect-licenses: {len(missing)} crates have no licence text in "
        f"{registry}; the artifact cannot ship without them.\n"
    )
    sys.exit(1)
PY

# The project's own terms, and the list of what the texts above belong to.
cp LICENSE "$out/LICENSE"
cp THIRD-PARTY-NOTICES.md "$out/THIRD-PARTY-NOTICES.md"

# Vendored Java libraries: their own LICENSE files, if the vendor tree carries them.
for lib in packages/mobile/vendor/*; do
    [ -d "$lib" ] || continue
    name=$(basename "$lib")
    mkdir -p "$out/vendor/$name"
    find "$lib" -maxdepth 2 -type f \( -iname 'LICENSE*' -o -iname 'COPYING*' \) \
        -exec cp {} "$out/vendor/$name/" \; 2>/dev/null || true
done

echo "wrote $out"
