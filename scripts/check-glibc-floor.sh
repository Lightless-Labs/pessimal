#!/usr/bin/env bash
# Asserts that a Linux binary asks no more of glibc than the support floor we promised.
#
# Usage: scripts/check-glibc-floor.sh <elf-binary> <max-glibc>
#        scripts/check-glibc-floor.sh target/release/pessimal-agent 2.28
#
# Exits non-zero naming every symbol whose required GLIBC version is above the floor, and always
# prints the full sorted set of versions the binary needs -- so the build log records the actual
# floor rather than a bare pass/fail. A binary that needs 2.17 and a binary that needs exactly 2.28
# are both passes at 2.28, but only one of them has headroom, and that is worth knowing before the
# next dependency bump eats it.
#
# Why an embedded Python ELF reader instead of a tool:
#   - `readelf`/`objdump` are not on a stock macOS, and the Linux binaries are cross-built with
#     cargo-zigbuild, so the Mac that builds them locally is where they are first inspected. A check
#     that can only run in the `release-linux` Buildkite step is a check nobody runs before tagging.
#     And the contents of that step's guest image were never inventoried, so a dependency on binutils
#     there would be a guess; python3 is checked for below instead.
#   - A `strings | grep GLIBC_` scan is not a cheap approximation of this, it is a different
#     question. It matches the bytes `GLIBC_` anywhere in the file -- a string literal in .rodata
#     counts as much as a real need -- and it cannot say which symbol wants a version, so a failure
#     could never name the offender. The dynamic loader enforces the `.gnu.version_r` version-need
#     table and nothing else, so that table is what this reads. (On the two binaries measured below
#     a strings scan happens to agree with the table; the planning notes' claim that it reports 2.30
#     did not reproduce against them. Agreement today is luck, not a property.)
# The floor is a real promise: glibc 2.28 is Debian 10, RHEL 8 and Ubuntu 18.10. Raising it silently
# drops every host older than whatever the builder happened to have.
#
# MEASURED on this repo's zigbuild output, 2026-09-13 -- the reference values that make this script
# testable on a Mac with no Docker:
#   target/x86_64-unknown-linux-gnu/release/pessimal-agent   needs at most GLIBC_2.28 (statx)
#   target/aarch64-unknown-linux-gnu/release/pessimal-agent  needs at most GLIBC_2.28 (statx)
# Both were built with cargo-zigbuild's `.2.28` target suffix, which caps the ABI, so both land on
# the same ceiling. That suffix is also why scripts/release-build-linux.sh builds the aarch64 binary
# through zigbuild even on an arm64 guest that could build it natively: a native `cargo build` links
# against the guest image's own glibc, and this script is what would catch the floor floating up.
#
# Note specifically that `gettid` is NOT a 2.30 requirement in either: Rust's std reaches it through
# `weak!`, so it appears in .dynsym as a WEAK undefined symbol at version index 1 (VER_NDX_GLOBAL)
# with no version need at all (re-read from the aarch64 binary's .dynsym/.gnu.version, 2026-09-13).
# It is the kind of symbol a strings scan would make you argue about and this table settles. The
# discriminating negative test is therefore a floor BELOW the real ceiling -- `... aarch64 ... 2.17`
# must fail naming __cxa_thread_atexit_impl@2.18, getrandom@2.25 and statx@2.28 -- not a 2.30
# gettid that is not there.
set -euo pipefail

die() { printf 'check-glibc-floor: %s\n' "$1" >&2; exit 1; }

binary="${1:-}"
floor="${2:-}"

[ -n "$binary" ] && [ -n "$floor" ] || die "usage: check-glibc-floor.sh <elf-binary> <max-glibc>"
[ -r "$binary" ] || die "cannot read $binary"
printf '%s' "$floor" | grep -Eq '^[0-9]+\.[0-9]+(\.[0-9]+)?$' \
  || die "floor must look like 2.28 or 2.2.5, got '$floor'"

# Named up front rather than letting the heredoc fail with "command not found": this runs inside the
# ci-linux-arm64-rust-bazel Tart guest, whose contents were never inventoried.
python3 -c 'import struct' >/dev/null 2>&1 \
  || die "a working python3 is required to read the ELF version-need table, and none runs here -- in the release-linux step that is the ci-linux-arm64-rust-bazel guest image"

python3 - "$binary" "$floor" <<'PY'
import struct
import sys

path, floor = sys.argv[1], sys.argv[2]
with open(path, "rb") as fh:
    data = fh.read()


def fail(msg):
    sys.stderr.write("check-glibc-floor: %s\n" % msg)
    raise SystemExit(1)


if data[:4] != b"\x7fELF":
    fail("%s is not an ELF file (a Mach-O or a shell script will look like this)" % path)
if data[4] != 2:
    fail("%s is a 32-bit ELF; only 64-bit targets are shipped, so this is the wrong file" % path)
endian = "<" if data[5] == 1 else ">"

# Section header table. Offsets are the fixed Elf64_Ehdr layout, not a guess.
(sh_off,) = struct.unpack_from(endian + "Q", data, 0x28)
sh_entsize, sh_num = struct.unpack_from(endian + "HH", data, 0x3A)

sections = []
for i in range(sh_num):
    base = sh_off + i * sh_entsize
    _, s_type, _, _, s_off, s_size, s_link, s_info = struct.unpack_from(
        endian + "IIQQQQII", data, base
    )
    sections.append(
        {"type": s_type, "off": s_off, "size": s_size, "link": s_link, "info": s_info}
    )

SHT_DYNSYM = 11
SHT_GNU_VERSYM = 0x6FFFFFFF
SHT_GNU_VERNEED = 0x6FFFFFFE


def find(section_type):
    for s in sections:
        if s["type"] == section_type:
            return s
    return None


def cstr(strtab_off, offset):
    start = strtab_off + offset
    return data[start : data.index(b"\0", start)].decode("utf-8", "replace")


verneed = find(SHT_GNU_VERNEED)
if verneed is None:
    # Never a vacuous pass. A statically linked or musl binary has no version needs at all, and so
    # does a file we failed to understand; those are different situations from "needs nothing above
    # the floor" and the caller has to be told which it got.
    fail(
        "%s has no .gnu.version_r section: nothing links it to a versioned glibc. "
        "A static or musl build is outside this check's remit; a dynamic glibc build that "
        "reaches this line is a file this script did not parse." % path
    )

strtab = sections[verneed["link"]]

# version index -> (version string, providing library). vna_other is the index the .gnu.version
# table uses to point a symbol at its required version.
by_index = {}
pos = verneed["off"]
for _ in range(verneed["info"]):
    _, vn_cnt, vn_file, vn_aux, vn_next = struct.unpack_from(endian + "HHIII", data, pos)
    aux = pos + vn_aux
    for _ in range(vn_cnt):
        _, _, vna_other, vna_name, vna_next = struct.unpack_from(endian + "IHHII", data, aux)
        by_index[vna_other & 0x7FFF] = (
            cstr(strtab["off"], vna_name),
            cstr(strtab["off"], vn_file),
        )
        if not vna_next:
            break
        aux += vna_next
    if not vn_next:
        break
    pos += vn_next


def parse(version):
    """GLIBC_2.2.5 -> (2, 2, 5). Tuples, because neither float nor string ordering works here:
    this table really does carry 2.2.5 and 2.3.4 alongside 2.9 and 2.28, and a string compare puts
    2.9 above 2.28 while a float chokes on the third component."""
    return tuple(int(part) for part in version.split("."))


floor_tuple = parse(floor)

dynsym = find(SHT_DYNSYM)
versym = find(SHT_GNU_VERSYM)
if dynsym is None or versym is None:
    fail("%s has a version-need table but no .dynsym/.gnu.version to attribute it to" % path)
dynstr = sections[dynsym["link"]]

# Every entry of the version-need table is a requirement, whether or not a symbol points at it:
# ld.so refuses to start a binary whose needed version the library lacks before it resolves a
# single symbol. So the table decides pass/fail, and .dynsym is walked only to put names on it.
needed = {entry: [] for entry in by_index.values()}

# .gnu.version runs in lockstep with .dynsym: entry i is the version index of symbol i. Only
# GLIBC_* needs are this script's business -- a GCC_* need from libgcc_s is real but is not the
# glibc floor, so it is reported as context and never as a failure.
ELF64_SYM_SIZE = 24
for i in range(dynsym["size"] // ELF64_SYM_SIZE):
    (raw_index,) = struct.unpack_from(endian + "H", data, versym["off"] + i * 2)
    entry = by_index.get(raw_index & 0x7FFF)
    if entry is None:
        continue
    (st_name,) = struct.unpack_from(endian + "I", data, dynsym["off"] + i * ELF64_SYM_SIZE)
    needed[entry].append(cstr(dynstr["off"], st_name))

glibc = sorted(
    ((parse(v[len("GLIBC_"):]), v, lib, syms) for (v, lib), syms in needed.items()
     if v.startswith("GLIBC_")),
)
other = sorted(set(v for (v, _lib) in needed if not v.startswith("GLIBC_")))

if not glibc:
    fail("%s has no GLIBC_* version need; this is not a glibc-dynamic binary" % path)


def names(syms):
    if not syms:
        return "no symbol binds to it, but the loader still requires it"
    return "%d symbol(s), e.g. %s" % (len(syms), ", ".join(sorted(syms)[:4]))


print("%s requires, by version:" % path)
for _tuple, version, library, syms in glibc:
    print("  %-14s %-22s %s" % (version, library, names(syms)))
if other:
    print("  (non-glibc version needs, not part of the floor: %s)" % ", ".join(other))

distinct = sorted(set((t, v) for t, v, _lib, _s in glibc))
print("distinct GLIBC versions required: %s" % " ".join(v for _t, v in distinct))
ceiling = distinct[-1][0]
print("highest required glibc: %s (floor allows %s)" % (".".join(map(str, ceiling)), floor))

# Every offender, not the first. A build that broke the floor in three places should say so once.
over = [(v, lib, syms) for t, v, lib, syms in glibc if t > floor_tuple]
if over:
    # Flushed first so a CI log shows the full version list before the verdict, not after it.
    sys.stdout.flush()
    sys.stderr.write(
        "check-glibc-floor: %s needs glibc above the %s floor:\n" % (path, floor)
    )
    for version, library, syms in over:
        if not syms:
            sys.stderr.write("  %s (%s): no symbol binds to it; the version-need entry itself\n"
                             % (version, library))
        for sym in sorted(syms):
            sys.stderr.write("  %s@%s (%s)\n" % (sym, version, library))
    sys.stderr.write(
        "check-glibc-floor: that would not run on the oldest host this release claims to support.\n"
    )
    raise SystemExit(1)

print("OK: nothing above GLIBC_%s" % floor)
PY
