"""Holds the console's layouts to bindings.rs: layout-check.py <bindings.rs> <sources…> -- <c++…>

bindgen computes sizes and alignments for aarch64 Linux (scripts/build-skia.sh). Each becomes a
static_assert that devkitA64's G++ (newlib, libstdc++ 16) compiles after each binding source. A
Rust name maps back to C++ as written, or with `::` before a capital and after a namespace, and
`<…>` for bindgen's `_open0_…_close0`; a type neither spelling names is counted, not checked.
Exits 1 on any difference, or when a source does not compile.
"""
import re
import subprocess
import sys
import tempfile

NAMESPACES = {"std", "skia", "textlayout", "skgpu", "sktext", "gpu", "skhdr", "skif"}
FACT = re.compile(r'\["(Size|Alignment) of (?:template specialization: )?(\w+)"\]\s*'
                  r"\[::core::mem::(?:size|align)_of::<[^\]]*?>\(\)\s*-\s*(\d+)usize\]")


def scoped(ident):
    parts = ident.split("_")
    if "" in parts:
        return ident
    out = parts[0]
    for prev, part in zip(parts, parts[1:]):
        out += ("::" if part[0].isupper() or prev in NAMESPACES else "_") + part
    return out


def spellings(name):
    templ = re.sub(r"_close\d", ">", re.sub(r"_open\d_", "<", name))
    return {templ, re.sub(r"\w+", lambda m: scoped(m.group()), templ)}


split = sys.argv.index("--")
sources, compiler = sys.argv[2:split], sys.argv[split + 1:]
lines, owner = [""], {}
for kind, name, value in set(FACT.findall(open(sys.argv[1]).read())):
    op = "sizeof" if kind == "Size" else "alignof"
    for cxx in spellings(name):
        owner[len(lines) + 1] = name
        lines.append(f'static_assert({op}({cxx}) == {value}, "{kind} of {name}");')

differ, checked = set(), set()
for source in sources:
    lines[0] = f'#include "{source}"'
    with tempfile.NamedTemporaryFile("w", suffix=".cpp") as f:
        f.write("\n".join(lines) + "\n")
        f.flush()
        run = subprocess.run([*compiler, "-fsyntax-only", f.name], capture_output=True, text=True)
        err = run.stderr
    if "compilation terminated" in err:
        sys.exit(err)
    failed, broken = set(), set()
    for line, msg in re.findall(rf"{re.escape(f.name)}:(\d+):\d+: error: (.*)", err):
        (failed if "static assertion failed" in msg else broken).add(int(line))
    differ |= {lines[n - 1] for n in failed}
    checked |= {owner[n] for n in owner if n not in broken}
for line in sorted(differ):
    print("layout differs:", line)
total = len(set(owner.values()))
print(f"{len(checked)} of {total} bindgen types checked against G++, {len(differ)} differ")
sys.exit(1 if differ or not checked else 0)
