#!/usr/bin/env python3
"""Applies the throw-away 'DoubleAdd' strategy (x * 2 as `add r, r`) used for the
maintainability check. Every replacement must match exactly once, or nothing is written."""
import os, sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
EDITS = {
    "crates/c0m-dos/src/lib.rs": [
        ("    /// Small leaf callee expanded at the call site.\n    Inline,\n",
         "    /// Small leaf callee expanded at the call site.\n    Inline,\n"
         "    /// Trial strategy: multiply by 2 as `add r, r`.\n    DoubleAdd,\n"),
        ("pub const ALL: [EmitStrategy; 8] = [", "pub const ALL: [EmitStrategy; 9] = ["),
        ("        Self::Inline,\n    ];", "        Self::Inline,\n        Self::DoubleAdd,\n    ];"),
        ("            | Self::Inline => 1,", "            | Self::Inline\n            | Self::DoubleAdd => 1,"),
        ("    if EmitStrategy::LeaChain.unlocked_at(tier) && matches!(c, 3 | 5 | 9) {",
         "    if EmitStrategy::DoubleAdd.unlocked_at(tier) && c == 2 {\n        EmitStrategy::DoubleAdd\n"
         "    } else if EmitStrategy::LeaChain.unlocked_at(tier) && matches!(c, 3 | 5 | 9) {"),
        ("assert_eq!(EmitStrategy::ALL.len(), 8);", "assert_eq!(EmitStrategy::ALL.len(), 9);"),
    ],
    "crates/c0m-emitter/src/lib.rs": [
        ("                EmitStrategy::ShiftAdd if c >= 2 && c.count_ones() <= 2 => {",
         "                EmitStrategy::DoubleAdd if c == 2 => {\n"
         "                    let x = self.emit_expr(other)?;\n"
         "                    let d = self.own(id, x)?;\n"
         '                    self.line(id, format!("add {0}, {0}", d.reg.name()));\n'
         "                    return Ok(d);\n"
         "                }\n"
         "                EmitStrategy::ShiftAdd if c >= 2 && c.count_ones() <= 2 => {"),
    ],
}

new = {}
for rel, edits in EDITS.items():
    text = open(os.path.join(ROOT, rel)).read()
    for old, rep in edits:
        if text.count(old) != 1:
            sys.exit(f"ABORTED: expected exactly one match in {rel} for:\n{old}\n(found {text.count(old)})")
        text = text.replace(old, rep)
    new[rel] = text
for rel, text in new.items():
    open(os.path.join(ROOT, rel), "w").write(text)
print("trial strategy applied to", ", ".join(new))
