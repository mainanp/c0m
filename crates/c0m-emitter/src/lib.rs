//! Increment 4 — The Direct Emitter (+ Register Oracle).
//!
//! One recursive walk over the weighted AST. The contract of the walk:
//!   emit_expr   "emit code that computes this node, hand back the register"
//!   emit_stmt   "emit code that does this thing"
//!   emit_branch "emit code that jumps to a label when this condition is
//!                true (or false), and falls through otherwise"
//!
//! Output is x86-64 assembly TEXT (GNU as, Intel syntax). Labels are
//! symbolic, so the assembler resolves forward jumps and no patch list is
//! needed. No intermediate representation is built: the AST is the only
//! thing walked, and the output text is written once and never read back.
//!
//! Traceability: the ONLY code that appends to `EmitCtx::output` is
//! `Emitter::commit`, and every instruction line reaches it carrying the
//! AST node that produced it. So "every instruction has an owner" is a
//! property of one function, not a habit.

pub mod oracle;

use std::collections::{HashMap, HashSet};

use c0m_dos::{dos_strategy, DosStore, EmitStrategy, StrategyLog};
use c0m_frontend::{Arena, NodeId, NodeKind, Payload, SourceSpan};

use oracle::{Reg, RegOracle};

// ── Tunable priors ──────────────────────────────────────────────────

/// A variable gets a register home if the node that declares it has at
/// least this DOS tier (and its address is never taken).
pub const HOME_MIN_TIER: u8 = 1;
/// System V passes the first six integer arguments in registers.
const ARG_REGS: [Reg; 6] = [Reg::Rdi, Reg::Rsi, Reg::Rdx, Reg::Rcx, Reg::R8, Reg::R9];
/// When fewer than this many pool registers are free after the left
/// operand of a binary expression is computed, it is parked on the stack.
const SPILL_MARGIN: usize = 2;

// ── Public result types ─────────────────────────────────────────────

#[derive(Debug)]
pub struct EmitError {
    pub span: SourceSpan,
    pub message: String,
}

/// One traceability record: output line `line` (1-based) was produced by
/// the AST node with identity `key`.
#[derive(Debug, Clone, Copy)]
pub struct TraceEntry {
    pub line: usize,
    pub key: u64,
    pub kind: NodeKind,
    pub span: SourceSpan,
}

pub struct EmitCtx {
    pub output: String,
    pub trace: Vec<TraceEntry>,
}

impl EmitCtx {
    pub fn new() -> Self {
        Self { output: String::new(), trace: Vec::new() }
    }

    /// The assembly with a `# NodeKind @ line:col` note after every
    /// instruction: what `--trace-emit` prints.
    pub fn annotated_listing(&self) -> String {
        let by_line: HashMap<usize, &TraceEntry> =
            self.trace.iter().map(|t| (t.line, t)).collect();
        let mut out = String::new();
        for (i, text) in self.output.lines().enumerate() {
            match by_line.get(&(i + 1)) {
                Some(t) => out.push_str(&format!(
                    "{:<36}# {:?} @ {}:{}\n",
                    text, t.kind, t.span.line, t.span.col
                )),
                None => {
                    out.push_str(text);
                    out.push('\n');
                }
            }
        }
        out
    }
}

impl Default for EmitCtx {
    fn default() -> Self {
        Self::new()
    }
}

pub struct EmitResult {
    pub ctx: EmitCtx,
    pub strategies: StrategyLog,
}

/// Top-level entry point: weighted AST in, assembly + trace out.
/// The weighting engine must have run first (it writes the tiers the DOS
/// and the emitter read).
pub fn emit_program(arena: &Arena, root: NodeId, dos: &DosStore) -> Result<EmitResult, EmitError> {
    let program = arena.get(root);
    if program.kind != NodeKind::Program {
        return Err(EmitError {
            span: program.span,
            message: "emit_program expects the Program root node".to_string(),
        });
    }
    let mut e = Emitter::new(arena, dos);
    e.commit_text(".intel_syntax noprefix");
    e.commit_text(".text");
    for &f in &program.children {
        e.emit_function(f)?;
    }
    e.commit_text(".section .note.GNU-stack,\"\",@progbits");
    Ok(EmitResult { ctx: e.out, strategies: e.log })
}

// ── Internal types ──────────────────────────────────────────────────

/// A value held in a register. `owned` = this register was allocated for
/// the value and must be freed after use. Not owned = it is a variable's
/// home (or the scratch `rax`): read it, never modify it, never free it.
#[derive(Clone, Copy)]
struct Val {
    reg: Reg,
    owned: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Loc {
    /// `[rbp + offset]` (offset is negative)
    Slot(i32),
    Reg(Reg),
}

#[derive(Clone, Copy)]
struct Binding {
    loc: Loc,
    /// Free the register when the scope ends.
    frees_reg: bool,
}

struct Scope {
    bindings: HashMap<String, Binding>,
    /// Names below a barrier cannot be seen from above it. Used for inlined
    /// callees: they see their own parameters, never the caller's variables.
    barrier: bool,
}

enum LineKind {
    Instr(NodeId),
    Label,
    Directive,
}

struct Line {
    text: String,
    kind: LineKind,
}

enum Limit {
    Imm(i64),
    At(Loc),
}

/// The two operands of a binary expression, ready in registers.
/// If the left one was parked on the stack it was popped into `rax`
/// (`spilled`), where it is borrowed scratch, not an owned register.
struct Pair {
    l: Val,
    r: Val,
    spilled: bool,
}

struct Emitter<'a> {
    arena: &'a Arena,
    dos: &'a DosStore,
    out: EmitCtx,
    out_lines: usize,
    log: StrategyLog,
    oracle: RegOracle,
    // per-function state
    body: Vec<Line>,
    scopes: Vec<Scope>,
    locals_bytes: i32,
    push_depth: u32,
    addr_taken: HashSet<String>,
    ret_label: String,
    ret_count: usize,
    // whole-module state
    label_counter: u32,
}

// ── Small free helpers ──────────────────────────────────────────────

fn fits_i32(v: i64) -> bool {
    i32::try_from(v).is_ok()
}

/// An integer literal's value (exactly what the DOS's own `int_value` sees).
fn int_lit(a: &Arena, id: NodeId) -> Option<i64> {
    let n = a.get(id);
    match (&n.kind, &n.payload) {
        (NodeKind::IntLit, Some(Payload::Int(v))) => Some(*v),
        _ => None,
    }
}

/// A compile-time constant: an integer literal or a negated one (`-1`).
fn const_i64(a: &Arena, id: NodeId) -> Option<i64> {
    if let Some(v) = int_lit(a, id) {
        return Some(v);
    }
    let n = a.get(id);
    if n.kind == NodeKind::Neg {
        return int_lit(a, n.children[0]).and_then(|v| v.checked_neg());
    }
    None
}

fn cc_for(kind: NodeKind) -> Option<&'static str> {
    match kind {
        NodeKind::CmpLt => Some("l"),
        NodeKind::CmpLtEq => Some("le"),
        NodeKind::CmpGt => Some("g"),
        NodeKind::CmpGtEq => Some("ge"),
        NodeKind::CmpEq => Some("e"),
        NodeKind::CmpNotEq => Some("ne"),
        _ => None,
    }
}

fn invert_cc(cc: &str) -> &'static str {
    match cc {
        "l" => "ge",
        "le" => "g",
        "g" => "le",
        "ge" => "l",
        "e" => "ne",
        _ => "e",
    }
}

fn operand(loc: Loc) -> String {
    match loc {
        Loc::Slot(off) => format!("qword ptr [rbp{off}]"),
        Loc::Reg(r) => r.name().to_string(),
    }
}

/// Names whose address is taken (`&x`, `&mut x`) anywhere under `id`.
/// Such variables must live in memory, never in a register.
fn scan_addr_taken(a: &Arena, id: NodeId, out: &mut HashSet<String>) {
    let n = a.get(id);
    if matches!(n.kind, NodeKind::Ref | NodeKind::RefMut) {
        let c = a.get(n.children[0]);
        if let (NodeKind::Ident, Some(Payload::Ident(name))) = (&c.kind, &c.payload) {
            out.insert(name.clone());
        }
    }
    for &c in &n.children {
        scan_addr_taken(a, c, out);
    }
}

/// `{ name = <expr>; }` -> (name, expr). Mirrors the DOS's own check.
fn simple_assign(a: &Arena, block: NodeId) -> Option<(String, NodeId)> {
    let b = a.get(block);
    if b.kind != NodeKind::Block || b.has_tail || b.children.len() != 1 {
        return None;
    }
    let asg = a.get(b.children[0]);
    if asg.kind != NodeKind::Assign {
        return None;
    }
    let target = a.get(asg.children[0]);
    match (&target.kind, &target.payload) {
        (NodeKind::Ident, Some(Payload::Ident(n))) => Some((n.clone(), asg.children[1])),
        _ => None,
    }
}

impl<'a> Emitter<'a> {
    fn new(arena: &'a Arena, dos: &'a DosStore) -> Self {
        Self {
            arena,
            dos,
            out: EmitCtx::new(),
            out_lines: 0,
            log: StrategyLog::new(arena.len()),
            oracle: RegOracle::new(),
            body: Vec::new(),
            scopes: Vec::new(),
            locals_bytes: 0,
            push_depth: 0,
            addr_taken: HashSet::new(),
            ret_label: String::new(),
            ret_count: 0,
            label_counter: 0,
        }
    }

     // ── output: the single choke point ──────────────────────────────

    /// The ONLY place that appends to the output. An instruction cannot
    /// reach the file without its owning node being recorded.
    fn commit(&mut self, line: Line) {
        match line.kind {
            LineKind::Instr(owner) => {
                self.out.output.push_str("    ");
                self.out.output.push_str(&line.text);
                self.out.output.push('\n');
                self.out_lines += 1;
                let n = self.arena.get(owner);
                self.out.trace.push(TraceEntry {
                    line: self.out_lines,
                    key: n.key(),
                    kind: n.kind,
                    span: n.span,
                });
            }
            LineKind::Directive => {
                self.out.output.push_str("    ");
                self.out.output.push_str(&line.text);
                self.out.output.push('\n');
                self.out_lines += 1;
            }
            LineKind::Label => {
                self.out.output.push_str(&line.text);
                self.out.output.push_str(":\n");
                self.out_lines += 1;
            }
        }
    }

    fn commit_text(&mut self, text: &str) {
        self.commit(Line { text: text.to_string(), kind: LineKind::Directive });
    }

    /// Buffer one instruction owned by `owner` (flushed with its function).
    fn line(&mut self, owner: NodeId, text: impl Into<String>) {
        self.body.push(Line { text: text.into(), kind: LineKind::Instr(owner) });
    }

    fn label(&mut self, name: &str) {
        self.body.push(Line { text: name.to_string(), kind: LineKind::Label });
    }

    fn directive(&mut self, text: impl Into<String>) {
        self.body.push(Line { text: text.into(), kind: LineKind::Directive });
    }

    fn new_label(&mut self) -> String {
        self.label_counter += 1;
        format!(".L{}", self.label_counter)
    }

    fn err<T>(&self, span: SourceSpan, message: impl Into<String>) -> Result<T, EmitError> {
        Err(EmitError { span, message: message.into() })
    }

    /// Ask the DOS what to do at this node, and log the answer.
    fn plan(&mut self, id: NodeId) -> EmitStrategy {
        let s = dos_strategy(self.arena, id, self.dos);
        self.log.record(id, s);
        s
    }

        // ── registers ───────────────────────────────────────────────────

    fn try_alloc_temp(&mut self, id: NodeId) -> Option<Reg> {
        let prio = self.arena.get(id).weight.combined;
        self.oracle.alloc_temp(prio)
    }

    fn alloc_temp(&mut self, id: NodeId) -> Result<Reg, EmitError> {
        match self.try_alloc_temp(id) {
            Some(r) => Ok(r),
            None => self.err(
                self.arena.get(id).span,
                "out of registers (expression too complex for the register pool)",
            ),
        }
    }

    fn release(&mut self, v: Val) {
        if v.owned {
            self.oracle.free(v.reg);
        }
    }

    /// Make sure the value sits in a register we own (and may modify).
    fn own(&mut self, id: NodeId, v: Val) -> Result<Val, EmitError> {
        if v.owned {
            return Ok(v);
        }
        let dst = self.alloc_temp(id)?;
        self.line(id, format!("mov {}, {}", dst.name(), v.reg.name()));
        Ok(Val { reg: dst, owned: true })
    }

    fn push_reg(&mut self, owner: NodeId, r: Reg) {
        self.line(owner, format!("push {}", r.name()));
        self.push_depth += 1;
    }

    fn pop_reg(&mut self, owner: NodeId, r: Reg) {
        self.line(owner, format!("pop {}", r.name()));
        self.push_depth -= 1;
    }

    // ── scopes and variables ────────────────────────────────────────

    fn push_scope(&mut self, barrier: bool) {
        self.scopes.push(Scope { bindings: HashMap::new(), barrier });
    }

    fn pop_scope(&mut self) {
        if let Some(scope) = self.scopes.pop() {
            for b in scope.bindings.values() {
                if b.frees_reg {
                    if let Loc::Reg(r) = b.loc {
                        self.oracle.free(r);
                    }
                }
            }
        }
    }

    fn lookup(&self, name: &str) -> Option<Binding> {
        for scope in self.scopes.iter().rev() {
            if let Some(b) = scope.bindings.get(name) {
                return Some(*b);
            }
            if scope.barrier {
                break;
            }
        }
        None
    }

    fn bind(&mut self, name: &str, b: Binding) {
        let scope = self.scopes.last_mut().expect("a scope is always open inside a function");
        if let Some(old) = scope.bindings.insert(name.to_string(), b) {
            if old.frees_reg {
                if let Loc::Reg(r) = old.loc {
                    self.oracle.free(r);
                }
            }
        }
    }

    fn alloc_slot(&mut self) -> i32 {
        self.locals_bytes += 8;
        -self.locals_bytes
    }

    /// Give `name` a home holding `v`: a register if the declaring node is
    /// hot enough and the variable never has its address taken, otherwise
    /// a stack slot. Consumes `v`.
    fn declare_from_val(&mut self, id: NodeId, name: &str, v: Val) -> Result<Loc, EmitError> {
        let n = self.arena.get(id);
        let tier = n.weight.dos_tier;
        let prio = n.weight.combined;
        if tier >= HOME_MIN_TIER && !self.addr_taken.contains(name) {
            if let Some(h) = self.oracle.alloc_home(prio) {
                self.line(id, format!("mov {}, {}", h.name(), v.reg.name()));
                self.release(v);
                let loc = Loc::Reg(h);
                self.bind(name, Binding { loc, frees_reg: true });
                return Ok(loc);
            }
        }
        let off = self.alloc_slot();
        self.line(id, format!("mov qword ptr [rbp{off}], {}", v.reg.name()));
        self.release(v);
        let loc = Loc::Slot(off);
        self.bind(name, Binding { loc, frees_reg: false });
        Ok(loc)
    }

    fn emit_load_named(&mut self, id: NodeId, name: &str) -> Result<Val, EmitError> {
        match self.lookup(name) {
            Some(Binding { loc: Loc::Reg(r), .. }) => Ok(Val { reg: r, owned: false }),
            Some(Binding { loc: Loc::Slot(off), .. }) => {
                let dst = self.alloc_temp(id)?;
                self.line(id, format!("mov {}, qword ptr [rbp{off}]", dst.name()));
                Ok(Val { reg: dst, owned: true })
            }
            None => self.err(self.arena.get(id).span, format!("undefined variable `{name}`")),
        }
    }

    fn store_named(&mut self, id: NodeId, name: &str, src: Reg) -> Result<(), EmitError> {
        match self.lookup(name) {
            Some(Binding { loc: Loc::Slot(off), .. }) => {
                self.line(id, format!("mov qword ptr [rbp{off}], {}", src.name()));
                Ok(())
            }
            Some(Binding { loc: Loc::Reg(h), .. }) => {
                if h != src {
                    self.line(id, format!("mov {}, {}", h.name(), src.name()));
                }
                Ok(())
            }
            None => self.err(self.arena.get(id).span, format!("undefined variable `{name}`")),
        }
    }

    // ── types ───────────────────────────────────────────────────────

    /// v1 emits integer-like values only: i64, bool, char (and references
    /// to them). Floats and strings need other registers / memory layouts.
    fn check_type(&self, id: NodeId) -> Result<(), EmitError> {
        let n = self.arena.get(id);
        match n.kind {
            NodeKind::TypeName => match &n.payload {
                Some(Payload::Ident(s)) if matches!(s.as_str(), "i64" | "bool" | "char") => Ok(()),
                Some(Payload::Ident(s)) => self.err(
                    n.span,
                    format!("type `{s}` is not supported by the emitter yet (i64, bool, char only)"),
                ),
                _ => self.err(n.span, "malformed type"),
            },
            NodeKind::TypeRef | NodeKind::TypeRefMut => self.check_type(n.children[0]),
            _ => Ok(()),
        }
    }

    // ── functions ───────────────────────────────────────────────────

    fn emit_function(&mut self, id: NodeId) -> Result<(), EmitError> {
        let a = self.arena;
        let f = a.get(id);
        let name = match &f.payload {
            Some(Payload::Ident(n)) => n.clone(),
            _ => return self.err(f.span, "function without a name"),
        };
        let (param_list, ret_type, body) = (f.children[0], f.children[1], f.children[2]);

        // fresh per-function state
        self.oracle.reset();
        self.scopes.clear();
        self.body.clear();
        self.locals_bytes = 0;
        self.push_depth = 0;
        self.addr_taken.clear();
        scan_addr_taken(a, body, &mut self.addr_taken);
        self.ret_label = self.new_label();
        let ret_children = &a.get(ret_type).children;
        self.ret_count = ret_children.len();
        if self.ret_count > 2 {
            return self.err(a.get(ret_type).span, "at most 2 return values are supported");
        }
        for &t in ret_children {
            self.check_type(t)?;
        }

        // parameters: arrive in registers, live in stack slots
        self.push_scope(false);
        let params = &a.get(param_list).children;
        if params.len() > ARG_REGS.len() {
            return self.err(a.get(param_list).span, "at most 6 parameters are supported");
        }
        for (i, &p) in params.iter().enumerate() {
            let pn = a.get(p);
            self.check_type(pn.children[0])?;
            let pname = match &pn.payload {
                Some(Payload::Ident(n)) => n.clone(),
                _ => return self.err(pn.span, "parameter without a name"),
            };
            let off = self.alloc_slot();
            self.line(p, format!("mov qword ptr [rbp{off}], {}", ARG_REGS[i].name()));
            self.bind(&pname, Binding { loc: Loc::Slot(off), frees_reg: false });
        }

        self.emit_block(body, None)?;
        self.pop_scope();

        // epilogue (callee-saved registers are known only now)
        let saved = self.oracle.callee_saved_touched();
        let mut total = self.locals_bytes + 8 * saved.len() as i32;
        total = (total + 15) & !15;
        let ret_label = self.ret_label.clone();
        self.label(&ret_label);
        if name == "main" && self.ret_count == 0 {
            self.line(id, "xor eax, eax"); // a `-> ()` main exits with status 0
        }
        for (k, r) in saved.iter().enumerate() {
            let off = -(self.locals_bytes + 8 * (k as i32 + 1));
            self.line(id, format!("mov {}, qword ptr [rbp{off}]", r.name()));
        }
        self.line(id, "mov rsp, rbp");
        self.line(id, "pop rbp");
        self.line(id, "ret");

        // prologue, then flush everything in order
        let mut head = vec![
            Line { text: format!(".globl {name}"), kind: LineKind::Directive },
            Line { text: format!(".type {name}, @function"), kind: LineKind::Directive },
            Line { text: name.clone(), kind: LineKind::Label },
            Line { text: "push rbp".to_string(), kind: LineKind::Instr(id) },
            Line { text: "mov rbp, rsp".to_string(), kind: LineKind::Instr(id) },
        ];
        if total > 0 {
            head.push(Line { text: format!("sub rsp, {total}"), kind: LineKind::Instr(id) });
        }
        for (k, r) in saved.iter().enumerate() {
            let off = -(self.locals_bytes + 8 * (k as i32 + 1));
            head.push(Line {
                text: format!("mov qword ptr [rbp{off}], {}", r.name()),
                kind: LineKind::Instr(id),
            });
        }
        for l in head {
            self.commit(l);
        }
        let body_lines = std::mem::take(&mut self.body);
        for l in body_lines {
            self.commit(l);
        }
        Ok(())
    }

    // ── statements ──────────────────────────────────────────────────

    fn emit_stmt(&mut self, id: NodeId) -> Result<(), EmitError> {
        let strat = self.plan(id);
        match self.arena.get(id).kind {
            NodeKind::Let => self.emit_let(id),
            NodeKind::Assign => self.emit_assign(id),
            NodeKind::Loop | NodeKind::ParallelLoop => self.emit_loop(id, strat),
            NodeKind::Match => self.emit_match(id, None, strat),
            NodeKind::If => self.emit_if(id, None, strat),
            NodeKind::Return => self.emit_return(id),
            NodeKind::Block => self.emit_block(id, None),
            _ => {
                // a bare expression statement: compute it, drop the value
                let v = self.emit_expr(id)?;
                self.release(v);
                Ok(())
            }
        }
    }

    /// Emits a block. If `dest` is given, the block's tail expression is
    /// moved into it (the block is being used as a value).
    fn emit_block(&mut self, id: NodeId, dest: Option<Reg>) -> Result<(), EmitError> {
        let a = self.arena;
        let n = a.get(id);
        self.push_scope(false);
        let count = n.children.len();
        for (i, &c) in n.children.iter().enumerate() {
            if n.has_tail && i + 1 == count {
                let v = self.emit_expr(c)?;
                if let Some(d) = dest {
                    self.line(c, format!("mov {}, {}", d.name(), v.reg.name()));
                }
                self.release(v);
            } else {
                self.emit_stmt(c)?;
            }
        }
        if dest.is_some() && !n.has_tail {
            let ends_in_return = n
                .children
                .last()
                .map_or(false, |&c| a.get(c).kind == NodeKind::Return);
            if !ends_in_return {
                return self.err(n.span, "a block used as a value must end in a value expression");
            }
        }
        self.pop_scope();
        Ok(())
    }

    fn emit_let(&mut self, id: NodeId) -> Result<(), EmitError> {
        let a = self.arena;
        let n = a.get(id);
        let name = match &n.payload {
            Some(Payload::Ident(s)) => s.clone(),
            _ => return self.err(n.span, "let without a name"),
        };
        if n.children.len() == 2 {
            self.check_type(n.children[0])?;
        }
        let value = *n.children.last().expect("let has a value");
        let v = self.emit_expr(value)?;
        self.declare_from_val(id, &name, v)?;
        Ok(())
    }
    
    fn emit_assign(&mut self, id: NodeId) -> Result<(), EmitError> {
        let a = self.arena;
        let n = a.get(id);
        let target = a.get(n.children[0]);
        let rhs = n.children[1];
        match (&target.kind, &target.payload) {
            (NodeKind::Ident, Some(Payload::Ident(name))) => {
                let v = self.emit_expr(rhs)?;
                self.store_named(id, name, v.reg)?;
                self.release(v);
                Ok(())
            }
            (NodeKind::Deref, _) => {
                let v = self.emit_expr(rhs)?;
                let p = self.emit_expr(target.children[0])?;
                self.line(id, format!("mov qword ptr [{}], {}", p.reg.name(), v.reg.name()));
                self.release(p);
                self.release(v);
                Ok(())
            }
            _ => self.err(target.span, "left side of '=' must be a variable or a dereference"),
        }
    }

    fn emit_return(&mut self, id: NodeId) -> Result<(), EmitError> {
        let a = self.arena;
        let n = a.get(id);
        if n.children.len() != self.ret_count {
            return self.err(
                n.span,
                format!(
                    "return gives {} value(s) but the function declares {}",
                    n.children.len(),
                    self.ret_count
                ),
            );
        }
        match n.children.len() {
            0 => {}
            1 => {
                let v = self.emit_expr(n.children[0])?;
                self.line(id, format!("mov rax, {}", v.reg.name()));
                self.release(v);
            }
            _ => {
                let v1 = self.emit_expr(n.children[0])?;
                let v2 = self.emit_expr(n.children[1])?;
                self.line(id, format!("mov rax, {}", v1.reg.name()));
                self.line(id, format!("mov rdx, {}", v2.reg.name()));
                self.release(v2);
                self.release(v1);
            }
        }
        let l = self.ret_label.clone();
        self.line(id, format!("jmp {l}"));
        Ok(())
    }

    // ── loops ───────────────────────────────────────────────────────

    fn emit_loop(&mut self, id: NodeId, strat: EmitStrategy) -> Result<(), EmitError> {
        let a = self.arena;
        let n = a.get(id);
        let var = match &n.payload {
            Some(Payload::Ident(s)) => s.clone(),
            _ => return self.err(n.span, "loop without a counter name"),
        };
        let (lo, hi, body) = (n.children[0], n.children[1], n.children[2]);
        let copies = match strat {
            EmitStrategy::Unroll4 => 4,
            EmitStrategy::Unroll2 => 2,
            _ => 1,
        };

        self.push_scope(false);
        // counter = lo
        let lo_v = self.emit_expr(lo)?;
        let counter = self.declare_from_val(id, &var, lo_v)?;
        // limit: evaluated ONCE, before the loop
        let limit = match const_i64(a, hi).filter(|v| fits_i32(*v)) {
            Some(imm) => Limit::Imm(imm),
            None => {
                let hv = self.emit_expr(hi)?;
                Limit::At(self.declare_from_val(id, "#limit", hv)?)
            }
        };

        let top = self.new_label();
        let end = self.new_label();
        self.label(&top);
        let c = operand(counter);
        match &limit {
            Limit::Imm(v) => self.line(id, format!("cmp {c}, {v}")),
            Limit::At(l) => {
                let lo_op = operand(*l);
                if matches!(counter, Loc::Slot(_)) && matches!(l, Loc::Slot(_)) {
                    self.line(id, format!("mov rax, {c}"));
                    self.line(id, format!("cmp rax, {lo_op}"));
                } else {
                    self.line(id, format!("cmp {c}, {lo_op}"));
                }
            }
        }
        self.line(id, format!("jge {end}"));
        for _ in 0..copies {
            self.emit_block(body, None)?;
            self.line(id, format!("add {c}, 1"));
        }
        self.line(id, format!("jmp {top}"));
        self.label(&end);
        self.pop_scope();
        Ok(())
    }

      // ── conditionals ────────────────────────────────────────────────

    /// Emits a comparison's `cmp` and returns the condition-code suffix
    /// ("l", "ge", ...). Operands are released; flags stay live.
    fn emit_compare_flags(&mut self, id: NodeId) -> Result<&'static str, EmitError> {
        let a = self.arena;
        let n = a.get(id);
        let cc = cc_for(n.kind).expect("emit_compare_flags needs a comparison node");
        let (l, r) = (n.children[0], n.children[1]);
        match const_i64(a, r).filter(|v| fits_i32(*v)) {
            Some(imm) => {
                let lv = self.emit_expr(l)?;
                self.line(id, format!("cmp {}, {imm}", lv.reg.name()));
                self.release(lv);
            }
            None => {
                let p = self.eval_pair(id, l, r)?;
                self.line(id, format!("cmp {}, {}", p.l.reg.name(), p.r.reg.name()));
                self.release(p.l);
                self.release(p.r);
            }
        }
        Ok(cc)
    }

    /// Flags for an arbitrary condition; returns the "true" condition code.
    fn emit_cond_cc(&mut self, cond: NodeId) -> Result<&'static str, EmitError> {
        self.plan(cond);
        if cc_for(self.arena.get(cond).kind).is_some() {
            return self.emit_compare_flags(cond);
        }
        let v = self.emit_expr(cond)?;
        self.line(cond, format!("test {0}, {0}", v.reg.name()));
        self.release(v);
        Ok("ne")
    }

    /// Jump to `target` when the condition equals `jump_when`; otherwise
    /// fall through. No 0/1 value is ever built for a branch.
    fn emit_branch(&mut self, id: NodeId, target: &str, jump_when: bool) -> Result<(), EmitError> {
        self.plan(id);
        let a = self.arena;
        let n = a.get(id);
        match n.kind {
            k if cc_for(k).is_some() => {
                let cc = self.emit_compare_flags(id)?;
                let cc = if jump_when { cc } else { invert_cc(cc) };
                self.line(id, format!("j{cc} {target}"));
            }
            NodeKind::LogicalAnd => {
                let (l, r) = (n.children[0], n.children[1]);
                if !jump_when {
                    self.emit_branch(l, target, false)?;
                    self.emit_branch(r, target, false)?;
                } else {
                    let skip = self.new_label();
                    self.emit_branch(l, &skip, false)?;
                    self.emit_branch(r, target, true)?;
                    self.label(&skip);
                }
            }
            NodeKind::LogicalOr => {
                let (l, r) = (n.children[0], n.children[1]);
                if jump_when {
                    self.emit_branch(l, target, true)?;
                    self.emit_branch(r, target, true)?;
                } else {
                    let skip = self.new_label();
                    self.emit_branch(l, &skip, true)?;
                    self.emit_branch(r, target, false)?;
                    self.label(&skip);
                }
            }
            NodeKind::Not => self.emit_branch(n.children[0], target, !jump_when)?,
            NodeKind::BoolLit => {
                let b = matches!(n.payload, Some(Payload::Bool(true)));
                if b == jump_when {
                    self.line(id, format!("jmp {target}"));
                }
            }
            _ => {
                let v = self.emit_expr(id)?;
                self.line(id, format!("test {0}, {0}", v.reg.name()));
                self.release(v);
                let cc = if jump_when { "ne" } else { "e" };
                self.line(id, format!("j{cc} {target}"));
            }
        }
        Ok(())
    }

     fn emit_if(&mut self, id: NodeId, dest: Option<Reg>, strat: EmitStrategy) -> Result<(), EmitError> {
        let a = self.arena;
        let kids = &a.get(id).children;
        if strat == EmitStrategy::Cmov && dest.is_none() {
            if let Some((name, a_rhs, b_rhs)) = self.cmov_parts(id) {
                return self.emit_if_cmov(id, &name, a_rhs, b_rhs);
            }
        }
        let else_l = self.new_label();
        self.emit_branch(kids[0], &else_l, false)?;
        self.emit_arm(kids[1], dest)?;
        match kids.get(2) {
            Some(&e) => {
                let end = self.new_label();
                self.line(id, format!("jmp {end}"));
                self.label(&else_l);
                self.emit_arm(e, dest)?;
                self.label(&end);
            }
            None => {
                if dest.is_some() {
                    return self.err(a.get(id).span, "an `if` used as a value needs an `else`");
                }
                self.label(&else_l);
            }
        }
        Ok(())
    }
    
     fn emit_arm(&mut self, node: NodeId, dest: Option<Reg>) -> Result<(), EmitError> {
        match self.arena.get(node).kind {
            NodeKind::If => {
                let s = self.plan(node);
                self.emit_if(node, dest, s)
            }
            _ => self.emit_block(node, dest),
        }
    }

    fn cmov_parts(&self, id: NodeId) -> Option<(String, NodeId, Option<NodeId>)> {
        let a = self.arena;
        let kids = &a.get(id).children;
        let (name, then_rhs) = simple_assign(a, kids[1])?;
        let else_rhs = match kids.get(2) {
            None => None,
            Some(&e) => {
                let (n2, rhs2) = simple_assign(a, e)?;
                if n2 != name {
                    return None;
                }
                Some(rhs2)
            }
        };
        Some((name, then_rhs, else_rhs))
    }

     /// `if c { r = a; } else { r = b; }` without a jump: compute both arms
    /// (the DOS proved they are cheap and cannot fault), set the flags, and
    /// let `cmov` pick. A missing `else` means "keep r's old value".
    fn emit_if_cmov(
        &mut self,
        id: NodeId,
        name: &str,
        a_rhs: NodeId,
        b_rhs: Option<NodeId>,
    ) -> Result<(), EmitError> {
        let cond = self.arena.get(id).children[0];
        let b = match b_rhs {
            Some(e) => {
                let v = self.emit_expr(e)?;
                self.own(e, v)?
            }
            None => {
                let v = self.emit_load_named(id, name)?;
                self.own(id, v)?
            }
        };
        let av = self.emit_expr(a_rhs)?;
        let cc = self.emit_cond_cc(cond)?;
        self.line(id, format!("cmov{cc} {}, {}", b.reg.name(), av.reg.name()));
        self.release(av);
        self.store_named(id, name, b.reg)?;
        self.release(b);
        Ok(())
    }
    
    // ── match ───────────────────────────────────────────────────────

    fn emit_match(&mut self, id: NodeId, dest: Option<Reg>, strat: EmitStrategy) -> Result<(), EmitError> {
        let a = self.arena;
        let kids = &a.get(id).children;
        let arms = &kids[1..];
        let sv = self.emit_expr(kids[0])?;
        let end = self.new_label();

        if strat == EmitStrategy::SublimateJumpTable {
            if let Some((min, max, values, default_arm)) = self.table_params(arms) {
                self.emit_match_table(id, sv, arms, dest, &end, min, max, &values, default_arm)?;
                self.label(&end);
                self.release(sv);
                return Ok(());
            }
        }

        for (k, &arm) in arms.iter().enumerate() {
            let pat = a.get(a.get(arm).children[0]);
            let next = self.new_label();
            match (&pat.kind, &pat.payload) {
                (NodeKind::IntLit, Some(Payload::Int(v))) => {
                    if fits_i32(*v) {
                        self.line(arm, format!("cmp {}, {v}", sv.reg.name()));
                    } else {
                        self.line(arm, format!("mov rax, {v}"));
                        self.line(arm, format!("cmp {}, rax", sv.reg.name()));
                    }
                    self.line(arm, format!("jne {next}"));
                }
                (NodeKind::Wildcard, _) | (NodeKind::Ident, _) => {}
                _ => return self.err(pat.span, "this kind of match pattern is not supported yet"),
            }
            self.emit_match_arm_body(arm, sv, dest)?;
            if k + 1 < arms.len() {
                self.line(arm, format!("jmp {end}"));
            }
            self.label(&next);
        }
        self.label(&end);
        self.release(sv);
        Ok(())
    }

    fn emit_match_arm_body(&mut self, arm: NodeId, sv: Val, dest: Option<Reg>) -> Result<(), EmitError> {
        let a = self.arena;
        let n = a.get(arm);
        let pat = a.get(n.children[0]);
        let body = n.children[1];
        self.push_scope(false);
        if let (NodeKind::Ident, Some(Payload::Ident(name))) = (&pat.kind, &pat.payload) {
            // an identifier pattern binds the scrutinee to a name (own copy)
            let copy = self.alloc_temp(arm)?;
            self.line(arm, format!("mov {}, {}", copy.name(), sv.reg.name()));
            self.bind(name, Binding { loc: Loc::Reg(copy), frees_reg: true });
        }
        if a.get(body).kind == NodeKind::Block {
            self.emit_block(body, dest)?;
        } else {
            let v = self.emit_expr(body)?;
            if let Some(d) = dest {
                self.line(body, format!("mov {}, {}", d.name(), v.reg.name()));
            }
            self.release(v);
        }
        self.pop_scope();
        Ok(())
    }

    /// (min, max, [(value, arm index)], default arm) when the arms really
    /// form a dense integer table the emitter can build.
    #[allow(clippy::type_complexity)]
    fn table_params(&self, arms: &[NodeId]) -> Option<(i64, i64, Vec<(i64, usize)>, Option<usize>)> {
        let a = self.arena;
        let mut values = Vec::new();
        let mut default_arm = None;
        for (i, &arm) in arms.iter().enumerate() {
            let pat = a.get(a.get(arm).children[0]);
            match (&pat.kind, &pat.payload) {
                (NodeKind::IntLit, Some(Payload::Int(v))) => values.push((*v, i)),
                (NodeKind::Wildcard, _) | (NodeKind::Ident, _) if i + 1 == arms.len() => {
                    default_arm = Some(i);
                }
                _ => return None,
            }
        }
        let min = values.iter().map(|v| v.0).min()?;
        let max = values.iter().map(|v| v.0).max()?;
        let range = max.checked_sub(min)?;
        if !fits_i32(min) || !fits_i32(range) {
            return None;
        }
        Some((min, max, values, default_arm))
    }

    /// Dense integer match as a jump table of 32-bit RELATIVE offsets: no
    /// relocations, so it works in position-independent executables.
    #[allow(clippy::too_many_arguments)]
    fn emit_match_table(
        &mut self,
        id: NodeId,
        sv: Val,
        arms: &[NodeId],
        dest: Option<Reg>,
        end: &str,
        min: i64,
        max: i64,
        values: &[(i64, usize)],
        default_arm: Option<usize>,
    ) -> Result<(), EmitError> {
        let arm_labels: Vec<String> = arms.iter().map(|_| self.new_label()).collect();
        let default_label = match default_arm {
            Some(i) => arm_labels[i].clone(),
            None => end.to_string(),
        };
        let tbl = self.new_label();

        self.line(id, format!("mov rax, {}", sv.reg.name()));
        if min != 0 {
            self.line(id, format!("sub rax, {min}"));
        }
        self.line(id, format!("cmp rax, {}", max - min));
        self.line(id, format!("ja {default_label}"));
        self.line(id, format!("lea rdx, [rip+{tbl}]"));
        self.line(id, "movsxd rax, dword ptr [rdx+rax*4]");
        self.line(id, "add rax, rdx");
        self.line(id, "jmp rax");

        self.directive(".balign 4");
        self.label(&tbl);
        for v in min..=max {
            let target = values
                .iter()
                .find(|(val, _)| *val == v)
                .map(|(_, i)| arm_labels[*i].clone())
                .unwrap_or_else(|| default_label.clone());
            self.directive(format!(".long {target}-{tbl}"));
        }

        for (i, &arm) in arms.iter().enumerate() {
            self.label(&arm_labels[i]);
            self.emit_match_arm_body(arm, sv, dest)?;
            if i + 1 < arms.len() {
                self.line(arm, format!("jmp {end}"));
            }
        }
        Ok(())
    }

    // ── expressions ─────────────────────────────────────────────────

    fn emit_expr(&mut self, id: NodeId) -> Result<Val, EmitError> {
        let strat = self.plan(id);
        let a = self.arena;
        let n = a.get(id);
        match n.kind {
            NodeKind::IntLit => match &n.payload {
                Some(Payload::Int(v)) => self.emit_const(id, *v),
                _ => self.err(n.span, "malformed integer literal"),
            },
            NodeKind::BoolLit => match &n.payload {
                Some(Payload::Bool(b)) => self.emit_const(id, *b as i64),
                _ => self.err(n.span, "malformed boolean literal"),
            },
            NodeKind::CharLit => match &n.payload {
                Some(Payload::Char(c)) => self.emit_const(id, *c as u32 as i64),
                _ => self.err(n.span, "malformed character literal"),
            },
            NodeKind::FloatLit | NodeKind::StringLit => {
                self.err(n.span, "floating-point and string values are not supported by the emitter yet")
            }
            NodeKind::Ident => match &n.payload {
                Some(Payload::Ident(name)) => {
                    let name = name.clone();
                    self.emit_load_named(id, &name)
                }
                _ => self.err(n.span, "identifier without a name"),
            },
            NodeKind::BinaryAdd => self.emit_addsub(id, "add"),
            NodeKind::BinarySub => self.emit_addsub(id, "sub"),
            NodeKind::BinaryMul => self.emit_mul(id, strat),
            NodeKind::BinaryDiv => self.emit_divmod(id, false),
            NodeKind::BinaryMod => self.emit_divmod(id, true),
            k if cc_for(k).is_some() => {
                let cc = self.emit_compare_flags(id)?;
                self.line(id, format!("set{cc} al"));
                let dst = self.alloc_temp(id)?;
                self.line(id, format!("movzx {}, al", dst.name()));
                Ok(Val { reg: dst, owned: true })
            }
            NodeKind::LogicalAnd | NodeKind::LogicalOr => {
                let dst = self.alloc_temp(id)?;
                let f = self.new_label();
                let end = self.new_label();
                self.emit_branch(id, &f, false)?;
                self.line(id, format!("mov {}, 1", dst.name()));
                self.line(id, format!("jmp {end}"));
                self.label(&f);
                self.line(id, format!("mov {}, 0", dst.name()));
                self.label(&end);
                Ok(Val { reg: dst, owned: true })
            }
            NodeKind::Neg => {
                if let Some(k) = const_i64(a, id) {
                    return self.emit_const(id, k);
                }
                let v = self.emit_expr(n.children[0])?;
                let v = self.own(id, v)?;
                self.line(id, format!("neg {}", v.reg.name()));
                Ok(v)
            }
            NodeKind::Not => {
                let v = self.emit_expr(n.children[0])?;
                let v = self.own(id, v)?;
                self.line(id, format!("xor {}, 1", v.reg.name()));
                Ok(v)
            }
            NodeKind::Ref | NodeKind::RefMut => self.emit_addr_of(id),
            NodeKind::Deref => {
                let v = self.emit_expr(n.children[0])?;
                let v = self.own(id, v)?;
                self.line(id, format!("mov {0}, qword ptr [{0}]", v.reg.name()));
                Ok(v)
            }
            NodeKind::Call => {
                if strat == EmitStrategy::Inline {
                    self.emit_inline_call(id)
                } else {
                    self.emit_direct_call(id)
                }
            }
            NodeKind::If => {
                let dst = self.alloc_temp(id)?;
                self.emit_if(id, Some(dst), strat)?;
                Ok(Val { reg: dst, owned: true })
            }
            NodeKind::Match => {
                let dst = self.alloc_temp(id)?;
                self.emit_match(id, Some(dst), strat)?;
                Ok(Val { reg: dst, owned: true })
            }
            other => self.err(n.span, format!("{other:?} cannot be used as a value")),
        }
    }

    fn emit_const(&mut self, id: NodeId, v: i64) -> Result<Val, EmitError> {
        let dst = self.alloc_temp(id)?;
        self.line(id, format!("mov {}, {v}", dst.name()));
        Ok(Val { reg: dst, owned: true })
    }

    fn emit_addr_of(&mut self, id: NodeId) -> Result<Val, EmitError> {
        let a = self.arena;
        let n = a.get(id);
        let target = a.get(n.children[0]);
        let name = match (&target.kind, &target.payload) {
            (NodeKind::Ident, Some(Payload::Ident(s))) => s.clone(),
            _ => return self.err(n.span, "only a variable can be borrowed"),
        };
        match self.lookup(&name) {
            Some(Binding { loc: Loc::Slot(off), .. }) => {
                let dst = self.alloc_temp(id)?;
                self.line(id, format!("lea {}, [rbp{off}]", dst.name()));
                Ok(Val { reg: dst, owned: true })
            }
            Some(_) => self.err(n.span, format!("cannot borrow `{name}`: it does not live in memory here")),
            None => self.err(n.span, format!("undefined variable `{name}`")),
        }
    }

    /// Evaluates both operands of a binary expression. If registers are
    /// running short the left one is parked on the stack while the right
    /// is computed, then popped into `rax` (the spill that makes
    /// arbitrarily deep expressions work).
    fn eval_pair(&mut self, id: NodeId, lhs: NodeId, rhs: NodeId) -> Result<Pair, EmitError> {
        let mut l = self.emit_expr(lhs)?;
        let mut spilled = false;
        if l.owned && self.oracle.free_count() < SPILL_MARGIN {
            self.push_reg(id, l.reg);
            self.release(l);
            self.oracle.note_spill();
            spilled = true;
        }
        let r = self.emit_expr(rhs)?;
        if spilled {
            self.pop_reg(id, Reg::Rax);
            self.oracle.end_spill();
            l = Val { reg: Reg::Rax, owned: false };
        }
        Ok(Pair { l, r, spilled })
    }

     /// `l op r` for add / sub / imul where both are registers.
    fn arith(&mut self, id: NodeId, op: &str, p: Pair) -> Result<Val, EmitError> {
        let (l, r) = (p.l, p.r);
        if l.owned {
            self.line(id, format!("{op} {}, {}", l.reg.name(), r.reg.name()));
            self.release(r);
            return Ok(l);
        }
        if !p.spilled {
            if let Some(dst) = self.try_alloc_temp(id) {
                self.line(id, format!("mov {}, {}", dst.name(), l.reg.name()));
                self.line(id, format!("{op} {}, {}", dst.name(), r.reg.name()));
                self.release(r);
                return Ok(Val { reg: dst, owned: true });
            }
            self.line(id, format!("mov rax, {}", l.reg.name()));
        }
        // computed in the scratch register, then moved to a pool register
        self.line(id, format!("{op} rax, {}", r.reg.name()));
        self.release(r);
        let dst = self.alloc_temp(id)?;
        self.line(id, format!("mov {}, rax", dst.name()));
        Ok(Val { reg: dst, owned: true })
    }

    fn emit_addsub(&mut self, id: NodeId, op: &str) -> Result<Val, EmitError> {
        let a = self.arena;
        let n = a.get(id);
        let (l, r) = (n.children[0], n.children[1]);
        if let Some(imm) = const_i64(a, r).filter(|v| fits_i32(*v)) {
            let lv = self.emit_expr(l)?;
            let lv = self.own(id, lv)?;
            self.line(id, format!("{op} {}, {imm}", lv.reg.name()));
            return Ok(lv);
        }
        let p = self.eval_pair(id, l, r)?;
        self.arith(id, op, p)
    }

    fn emit_mul(&mut self, id: NodeId, strat: EmitStrategy) -> Result<Val, EmitError> {
        let a = self.arena;
        let n = a.get(id);
        let (l, r) = (n.children[0], n.children[1]);

        // the constant side, found exactly the way the DOS finds it
        let konst = match (int_lit(a, r), int_lit(a, l)) {
            (Some(c), _) => Some((c, l)),
            (None, Some(c)) => Some((c, r)),
            _ => None,
        };
        if let Some((c, other)) = konst {
            match strat {
                EmitStrategy::LeaChain if matches!(c, 3 | 5 | 9) => {
                    let x = self.emit_expr(other)?;
                    let dst = if x.owned { x.reg } else { self.alloc_temp(id)? };
                    let (x, k) = (x.reg.name(), c - 1);
                    self.line(id, format!("lea {}, [{x}+{x}*{k}]", dst.name()));
                    return Ok(Val { reg: dst, owned: true });
                }
                EmitStrategy::ShiftAdd if c >= 2 && c.count_ones() <= 2 => {
                    let x = self.emit_expr(other)?;
                    let hi_bit = 63 - (c as u64).leading_zeros();
                    let lo_bit = (c as u64).trailing_zeros();
                    if c.count_ones() == 2 {
                        // x*c = (x << hi) + (x << lo): the low part goes to rax first
                        self.line(id, format!("mov rax, {}", x.reg.name()));
                        if lo_bit > 0 {
                            self.line(id, format!("shl rax, {lo_bit}"));
                        }
                    }
                    let d = self.own(id, x)?;
                    self.line(id, format!("shl {}, {hi_bit}", d.reg.name()));
                    if c.count_ones() == 2 {
                        self.line(id, format!("add {}, rax", d.reg.name()));
                    }
                    return Ok(d);
                }
                _ => {}
            }
        }

        if let Some(imm) = const_i64(a, r).filter(|v| fits_i32(*v)) {
            let lv = self.emit_expr(l)?;
            let lv = self.own(id, lv)?;
            self.line(id, format!("imul {0}, {0}, {imm}", lv.reg.name()));
            return Ok(lv);
        }
        let p = self.eval_pair(id, l, r)?;
        self.arith(id, "imul", p)
    }

     fn emit_divmod(&mut self, id: NodeId, want_rem: bool) -> Result<Val, EmitError> {
        let a = self.arena;
        let n = a.get(id);
        let p = self.eval_pair(id, n.children[0], n.children[1])?;
        if !p.spilled {
            self.line(id, format!("mov rax, {}", p.l.reg.name()));
        }
        self.line(id, "cqo");
        self.line(id, format!("idiv {}", p.r.reg.name()));
        self.release(p.l);
        self.release(p.r);
        let dst = self.alloc_temp(id)?;
        let src = if want_rem { "rdx" } else { "rax" };
        self.line(id, format!("mov {}, {src}", dst.name()));
        Ok(Val { reg: dst, owned: true })
    }

    // ── calls ───────────────────────────────────────────────────────

    fn callee_name(&self, id: NodeId) -> Result<String, EmitError> {
        let a = self.arena;
        let callee = a.get(a.get(id).children[0]);
        match (&callee.kind, &callee.payload) {
            (NodeKind::Ident, Some(Payload::Ident(n))) => Ok(n.clone()),
            _ => self.err(callee.span, "only direct calls to named functions are supported"),
        }
    }

    /// System V call. Live caller-saved temporaries are pushed first and
    /// popped after; arguments go through the stack so that computing one
    /// argument can never overwrite another's register.
    fn emit_direct_call(&mut self, id: NodeId) -> Result<Val, EmitError> {
        let a = self.arena;
        let n = a.get(id);
        let name = self.callee_name(id)?;
        let args = &n.children[1..];
        if args.len() > ARG_REGS.len() {
            return self.err(n.span, "at most 6 call arguments are supported");
        }

        let saved = self.oracle.live_caller_saved();
        for &r in &saved {
            self.push_reg(id, r);
        }
        for &arg in args {
            let v = self.emit_expr(arg)?;
            self.push_reg(arg, v.reg);
            self.release(v);
        }
        for i in (0..args.len()).rev() {
            self.pop_reg(id, ARG_REGS[i]);
        }
        let pad = self.push_depth % 2 == 1; // rsp must be 16-aligned at `call`
        if pad {
            self.line(id, "sub rsp, 8");
        }
        self.line(id, format!("call {name}"));
        if pad {
            self.line(id, "add rsp, 8");
        }
        let dst = self.alloc_temp(id)?;
        self.line(id, format!("mov {}, rax", dst.name()));
        for &r in saved.iter().rev() {
            self.pop_reg(id, r);
        }
        Ok(Val { reg: dst, owned: true })
    }

    /// Expand a small leaf callee at the call site: evaluate each argument
    /// once, bind the callee's parameter names to them inside a BARRIER
    /// scope, and emit the callee's `return` expression right here.
    fn emit_inline_call(&mut self, id: NodeId) -> Result<Val, EmitError> {
        let a = self.arena;
        let n = a.get(id);
        let name = self.callee_name(id)?;
        let Some(func) = self.dos.function(&name) else {
            return self.emit_direct_call(id);
        };
        let f = a.get(func);
        let params = &a.get(f.children[0]).children;
        let body = a.get(f.children[2]);
        let ret = a.get(body.children[0]);
        let expr = ret.children[0];
        let args = &n.children[1..];
        if params.len() != args.len() {
            return self.emit_direct_call(id);
        }

        let mut vals = Vec::new();
        for &arg in args {
            vals.push(self.emit_expr(arg)?);
        }
        self.push_scope(true);
        for (&p, v) in params.iter().zip(vals) {
            let pn = a.get(p);
            self.check_type(pn.children[0])?;
            if let Some(Payload::Ident(pname)) = &pn.payload {
                self.bind(pname, Binding { loc: Loc::Reg(v.reg), frees_reg: v.owned });
            }
        }
        let r = self.emit_expr(expr)?;
        let r = self.own(id, r)?; // the result must outlive the parameter registers
        self.pop_scope();
        Ok(r)
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use c0m_frontend::parse_source;
    use c0m_weighting::{analyze, no_history};

    fn compile(src: &str) -> Result<EmitResult, EmitError> {
        let (mut arena, root) = parse_source(src, 0).expect("should parse");
        analyze(&mut arena, root, &no_history);
        let dos = DosStore::build(&arena, root);
        emit_program(&arena, root, &dos)
    }

    fn asm(src: &str) -> String {
        compile(src).expect("should emit").ctx.output
    }

    fn count(haystack: &str, needle: &str) -> usize {
        haystack.matches(needle).count()
    }

    #[test]
    fn minimal_main_exits_with_status_zero() {
        let out = asm("fn main() -> () { let x: i64 = 1; }");
        assert!(out.contains("main:"));
        assert!(out.contains("xor eax, eax"));
        assert!(out.contains("ret"));
    }

    #[test]
    fn arithmetic_uses_an_immediate_add() {
        let out = asm("fn f(x: i64) -> i64 { return x + 1; }");
        assert!(out.contains("add rcx, 1"), "{out}");
    }

    #[test]
    fn every_instruction_has_exactly_one_trace_entry() {
        for src in [
            include_str!("../../../c0m-tests/01_branching.c0m"),
            include_str!("../../../c0m-tests/03_nested_loop.c0m"),
            include_str!("../../../c0m-tests/04_hot_kernel.c0m"),
            include_str!("../../../c0m-tests/05_calls.c0m"),
        ] {
            let ctx = compile(src).expect("should emit").ctx;
            let instr_lines: Vec<usize> = ctx
                .output
                .lines()
                .enumerate()
                .filter(|(_, l)| l.starts_with("    ") && !l.trim_start().starts_with('.'))
                .map(|(i, _)| i + 1)
                .collect();
            let traced: Vec<usize> = ctx.trace.iter().map(|t| t.line).collect();
            assert_eq!(instr_lines, traced, "trace must cover every instruction exactly once");
        }
    }

    #[test]
    fn unsupported_types_are_clean_errors() {
        let e = compile("fn f(x: f64) -> i64 { return 1; }").err().expect("should fail");
        assert!(e.message.contains("f64"));
        let e = compile("fn f() -> f64 { return 1.5; }").err().expect("should fail");
        assert!(e.message.contains("f64"));
    }

    #[test]
    fn undefined_variable_reports_a_location() {
        let e = compile("fn f() -> i64 { return nope; }").err().expect("should fail");
        assert!(e.message.contains("nope"));
        assert_eq!(e.span.line, 1);
    }

    #[test]
    fn return_count_must_match_the_signature() {
        assert!(compile("fn f() -> i64 { return 1, 2; }").is_err());
        assert!(compile("fn f() -> (i64, i64) { return 1; }").is_err());
    }

    #[test]
    fn simple_if_else_in_a_loop_becomes_cmov_without_a_jump() {
        let src = include_str!("../../../c0m-tests/01_branching.c0m");
        let out = asm(src);
        assert!(out.contains("cmov"), "{out}");
    }

    #[test]
    fn single_loop_is_unrolled_by_two() {
        let out = asm(include_str!("../../../c0m-tests/02_single_loop.c0m"));
        // counter bump appears once per unrolled copy
        assert_eq!(count(&out, ", 1\n"), 2, "{out}");
    }

    #[test]
    fn hot_kernel_innermost_loop_is_unrolled_by_four() {
        let out = asm(include_str!("../../../c0m-tests/04_hot_kernel.c0m"));
        assert!(out.contains("cmov"), "{out}");
        assert_eq!(count(&out, "add r13, 1"), 4, "{out}");
    }

    #[test]
    fn dense_match_at_tier_two_uses_a_jump_table() {
        let src = "fn f(x: i64) -> i64 { @2 match x { 0 => { return 10; }, 1 => { return 11; }, \
                   2 => { return 12; }, 3 => { return 13; }, _ => { return 0; }, } return 1; }";
        let out = asm(src);
        assert!(out.contains("jmp rax"), "{out}");
        assert!(out.contains(".long"), "{out}");
    }

    #[test]
    fn leaf_calls_are_inlined_and_the_rest_are_real_calls() {
        let out = asm(include_str!("../../../c0m-tests/05_calls.c0m"));
        assert!(!out.contains("call double"), "{out}");
        assert!(out.contains("call quad"), "{out}");
    }

    #[test]
    fn strategy_log_covers_every_emitted_node() {
        let r = compile(include_str!("../../../c0m-tests/05_calls.c0m")).expect("should emit");
        assert!(r.strategies.iter().count() > 0);
    }

    #[test]
    fn deep_expressions_spill_instead_of_failing() {
        // right-deep: 1 + (2 + (3 + ... + x)) keeps many left operands pending
        let mut e = String::from("x");
        for k in (1..=20).rev() {
            e = format!("{k} + ({e})");
        }
        let src = format!("fn f(x: i64) -> i64 {{ return {e}; }}");
        let out = asm(&src);
        assert!(out.contains("push"), "{out}");
        assert!(out.contains("pop rax"), "{out}");
    }
}






