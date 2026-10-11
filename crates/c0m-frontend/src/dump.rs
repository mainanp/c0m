//! Human readable AST dump, for development and for --dump-ast flag

use crate::{Arena, NodeId, Payload};

/// The tree shape only: kinds, payloads, positions, hints.
pub fn dump_tree(arena: &Arena, root: NodeId) -> String {
    let mut out = String::new();
    dump_node(arena, root, 0, false, &mut out);
    out
}

/// Same, plus each node's weights after the weighting engine has run:
/// `raw` (structure), `learned` (only shown when non-zero), `comb`
/// (combined) and `T0`-`T3` (the DOS tier the node ended up with).
pub fn dump_tree_weighted(arena: &Arena, root: NodeId) -> String {
    let mut out = String::new();
    dump_node(arena, root, 0, true, &mut out);
    out
}

fn dump_node(arena: &Arena, id: NodeId, depth: usize, show_weights: bool, out: &mut String) {
    let n = arena.get(id);

    let payload = match &n.payload {
        Some(Payload::Int(v)) => format!(" {v}"),
        Some(Payload::Float(v)) => format!(" {v}"),
        Some(Payload::Str(s)) => format!(" {s:?}"),
        Some(Payload::Char(c)) => format!(" {c:?}"),
        Some(Payload::Bool(b)) => format!(" {b}"),
        Some(Payload::Ident(s)) => format!(" `{s}`"),
        None => String::new(),
    };

    let mut notes = format!("{}:{}", n.span.line, n.span.col);
    if let Some(h) = n.loop_bound_hint {
        notes.push_str(&format!(" bound={h}"));
    }
    if let Some(t) = n.tier_override {
        notes.push_str(&format!(" tier=@{t}"));
    }
    if n.has_tail {
        notes.push_str(" tail");
    }

    if show_weights {
        let w = &n.weight;
        notes.push_str(&format!(" | raw={:.1}", w.raw));
        if w.learned != 0.0 {
            notes.push_str(&format!(" learned={:.1}", w.learned));
        }
        notes.push_str(&format!(" comb={:.1} T{}", w.combined, w.dos_tier));
    }

    out.push_str(&format!(
        "{}{:?}{}  [{}]\n",
        "  ".repeat(depth),
        n.kind,
        payload,
        notes
    ));

    for &child in &n.children {
        dump_node(arena, child, depth + 1, show_weights, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_source;

    #[test]
    fn weighted_dump_shows_weights_and_plain_dump_does_not() {
        let (mut arena, root) = parse_source("fn main() -> () { }", 0).expect("should parse");
        {
            let node = arena.get_mut(root);
            node.weight.raw = 6.0;
            node.weight.combined = 6.0;
            node.weight.dos_tier = 1;
        }
        let plain = dump_tree(&arena, root);
        let weighted = dump_tree_weighted(&arena, root);
        assert!(!plain.contains("raw="));
        assert!(weighted.contains("raw=6.0 comb=6.0 T1"));
    }
}
