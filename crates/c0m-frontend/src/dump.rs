//! Human readable AST dump, for development and for --dump-ast flag

use crate::{Arena, NodeId, Payload};

pub fn dump_tree(arena: &Arena, root: NodeId) -> String {
    let mut out = String::new();
    dump_node(arena, root, 0, &mut out);
    out
} 

fn dump_node(arena: &Arena, id: NodeId, depth: usize, out: &mut String) {
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

    out.push_str(&format!(
        "{}{:?}{}  [{}]\n",
        "  ".repeat(depth),
        n.kind,
        payload,
        notes
    ));

    for &child in &n.children {
        dump_node(arena, child, depth + 1, out);
    }


}
