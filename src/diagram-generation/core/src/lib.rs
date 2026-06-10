use mermaid_builder::prelude::*;
use ra_ap_syntax::{
    ast::{self, AstNode, HasArgList, HasGenericArgs, HasName},
    match_ast, Edition, SourceFile, SyntaxKind, SyntaxNode, SyntaxToken, T,
};
use std::collections::{HashMap, HashSet};

#[derive(Debug)]
pub struct FsmDefinition {
    pub name: String,
    pub context_type: Option<String>,
    pub states: Vec<StateDefinition>,
}

#[derive(Debug)]
pub struct StateDefinition {
    pub name: String,
    pub fields: Vec<(String, String)>,
    pub entry_block: Option<String>,
    pub process_block: String,
    pub exit_block: Option<String>,
}

/// Errors produced by [`parse_macro_body`] when a `state_machine!` invocation
/// is malformed or incomplete enough that no useful diagram can be generated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// The macro body had no `Name:` field.
    MissingName,
    /// The macro produced no usable states. Either the `States:` block was
    /// missing, empty, or every state inside it was malformed.
    EmptyStates,
    /// A state was declared (its name and optional fields were parsed) but
    /// no `process:` block was found inside its body.
    StateMissingProcessBlock(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingName => {
                write!(f, "state_machine! macro is missing the `Name:` field")
            }
            Self::EmptyStates => write!(
                f,
                "state_machine! macro has no states (missing or empty `States:` block)"
            ),
            Self::StateMissingProcessBlock(s) => {
                write!(f, "state `{}` has no `process:` block", s)
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// Errors surfaced while building a Mermaid diagram from a [`FsmDefinition`].
/// Wraps the underlying `mermaid-builder` error as a string so callers don't
/// need to depend on its concrete error type.
#[derive(Debug, Clone)]
pub enum DiagramError {
    Build(String),
}

impl std::fmt::Display for DiagramError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Build(s) => write!(f, "diagram builder error: {}", s),
        }
    }
}

impl std::error::Error for DiagramError {}

/// Convert any displayable error into a [`DiagramError::Build`].
fn build_err<E: std::fmt::Display>(e: E) -> DiagramError {
    DiagramError::Build(e.to_string())
}

/// Canonicalize a snippet of Rust source — collapse whitespace, expand
/// short-form operators (`&&` → ` and `), tighten punctuation. Produces a
/// string suitable for use as a logical event/guard fragment.
///
/// Presentation concerns (HTML line breaks, Mermaid escapes) live in
/// [`format_label_for_mermaid`] — not here.
pub fn normalize_source(s: String) -> String {
    let mut res = s.replace('\n', " ").replace('\r', " ");

    // logical and comparison operators
    res = res
        .replace("&&", " and ")
        .replace("||", " or ")
        .replace("==", " == ")
        .replace("!=", " != ")
        .replace(">=", " >= ")
        .replace("<=", " <= ");

    // Ensure mashed punctuation is separated
    res = res
        .replace("{", " { ")
        .replace("}", " } ")
        .replace(",", " , ")
        .replace("|", " | ");

    // Normalize spaces
    let tokens: Vec<_> = res.split_whitespace().collect();
    let mut res = tokens.join(" ");

    // Tighten the canonical form.
    res = res
        .replace(" : : ", "::")
        .replace(" :: ", "::")
        .replace(" : ", ":")
        .replace(" ( ", "(")
        .replace(" (", "(")
        .replace("( ", "(")
        .replace(" ) ", ")")
        .replace(" )", ")")
        .replace(" , ", ", ")
        .replace(", }", " }")
        .replace(" . ", ".")
        .replace(" .", ".")
        .replace(". ", ".")
        .replace("! ", "!");

    res.trim().to_string()
}

/// A transition's label as two structured lists. Built up during extraction,
/// rendered to Mermaid (or some other backend) at the end.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct TransitionLabel {
    /// Event-side fragments — patterns like `Event::Press`, `Idle`, or
    /// `let Event::X = evt`. Joined with a single space at render time.
    pub events: Vec<String>,
    /// Guard-side fragments — boolean conditions like `ctx.ready` or `n > 0`.
    /// Joined with ` and ` at render time and wrapped in `[ if … ]`.
    pub guards: Vec<String>,
}

impl TransitionLabel {
    pub fn is_empty(&self) -> bool {
        self.events.is_empty() && self.guards.is_empty()
    }

    fn push_event(&mut self, s: String) {
        if !s.is_empty() {
            self.events.push(s);
        }
    }

    fn push_guard(&mut self, s: String) {
        if !s.is_empty() {
            self.guards.push(s);
        }
    }
}

/// Render a [`TransitionLabel`] for a Mermaid edge. Inserts the `<br/>` line
/// break in front of brace-destructured patterns, joins the guard list, and
/// escapes `:` as `#colon;` so Mermaid doesn't truncate at the colon.
pub fn format_label_for_mermaid(label: &TransitionLabel) -> String {
    if label.is_empty() {
        // Mermaid requires *some* label text on an edge; use `*` as the
        // "no specific trigger" marker — matches the prior render behavior.
        return "*".to_string();
    }

    let events = label.events.join(" ");
    let combined = if label.guards.is_empty() {
        events
    } else {
        let guards = label.guards.join(" and ");
        if events.is_empty() {
            format!("[ if {} ]", guards)
        } else {
            format!("{}<br/>[ if {} ]", events, guards)
        }
    };

    // Visual: put destructured-field braces on their own line.
    let combined = combined.replace(" {", "<br/>{");
    // Mermaid escape: `:` is a syntax character inside edge labels.
    combined.replace(':', "#colon;")
}

pub struct TransitionInfo {
    pub source: String,
    pub target: String,
    pub label: TransitionLabel,
}

/// What we know about one function definition that might be worth following
/// when building a state diagram.
#[derive(Debug, Clone, Default)]
pub struct FunctionInfo {
    /// True if the function's declared return type syntactically mentions
    /// `Transition`. Helpers that don't return `Transition<...>` can't
    /// possibly drive a state change, so we don't follow them.
    pub returns_transition: bool,
    /// Raw path text of each `Transition::To(target)` argument seen inside the
    /// body (e.g. `"Self::Idle"`, `"MyFsm::Ready"`). The follow site checks
    /// that the prefix matches its own FSM before emitting an edge.
    pub transition_targets: HashSet<String>,
}

/// Registry of every function definition the scanner saw, keyed by short
/// name. When the same name appears twice (different modules, different
/// impls), the lookup deliberately refuses to disambiguate — guessing the
/// wrong one produces phantom edges in the diagram.
#[derive(Debug, Default)]
pub struct FunctionRegistry {
    by_name: HashMap<String, Vec<FunctionInfo>>,
}

impl FunctionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&mut self, name: String, info: FunctionInfo) {
        self.by_name.entry(name).or_default().push(info);
    }

    /// Look up a function by name. Returns `Some` only if exactly one
    /// definition matches — collisions return `None` so the caller can
    /// safely skip rather than picking arbitrarily.
    pub fn lookup_unambiguous(&self, name: &str) -> Option<&FunctionInfo> {
        match self.by_name.get(name)?.as_slice() {
            [single] => Some(single),
            _ => None,
        }
    }

    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

/// Recursively check whether `ty` syntactically mentions a path segment named
/// `Transition`. Walks through generic args (so `Result<Transition<Self>>`
/// matches), references (`&Transition<Self>`), tuples, slices, and arrays.
///
/// Substring matching on the type text used to do this — but it fired on
/// types like `MyTransition` too. This structural version only matches the
/// exact segment name.
fn type_mentions_transition(ty: &ast::Type) -> bool {
    match ty {
        ast::Type::PathType(p) => {
            let Some(path) = p.path() else { return false };
            for segment in path.segments() {
                if segment
                    .name_ref()
                    .is_some_and(|n| n.text() == "Transition")
                {
                    return true;
                }
                if let Some(args) = segment.generic_arg_list() {
                    for arg in args.generic_args() {
                        if let ast::GenericArg::TypeArg(t) = arg {
                            if let Some(inner) = t.ty() {
                                if type_mentions_transition(&inner) {
                                    return true;
                                }
                            }
                        }
                    }
                }
            }
            false
        }
        ast::Type::TupleType(t) => t.fields().any(|f| type_mentions_transition(&f)),
        ast::Type::RefType(r) => r.ty().is_some_and(|t| type_mentions_transition(&t)),
        ast::Type::ParenType(p) => p.ty().is_some_and(|t| type_mentions_transition(&t)),
        ast::Type::ArrayType(a) => a.ty().is_some_and(|t| type_mentions_transition(&t)),
        ast::Type::SliceType(s) => s.ty().is_some_and(|t| type_mentions_transition(&t)),
        _ => false,
    }
}

/// Produce a canonical, whitespace-free key for `ty`, suitable for use in
/// alias / struct-field registries and the `resolve_type` chain.
///
/// `PathType` segments are joined with `::` and *generic arguments are
/// stripped* — so `Vec<MyFsm>` becomes `Vec`, not `Vec<MyFsm>`. Downstream
/// the consumer takes the last `::` segment as the base type name; keeping
/// generics in the key would defeat that.
///
/// Reference wrappers are preserved (`&Foo` → `&Foo`) so aliases written
/// with `&` semantics still match. For anything unusual the fallback is the
/// raw whitespace-stripped source text.
pub fn canonical_type_text(ty: &ast::Type) -> String {
    match ty {
        ast::Type::PathType(p) => {
            if let Some(path) = p.path() {
                let segments: Vec<String> = path
                    .segments()
                    .filter_map(|s| s.name_ref().map(|n| n.text().to_string()))
                    .collect();
                if !segments.is_empty() {
                    return segments.join("::");
                }
            }
            ty.syntax().text().to_string().replace(' ', "")
        }
        ast::Type::RefType(r) => {
            let inner = r
                .ty()
                .map(|t| canonical_type_text(&t))
                .unwrap_or_default();
            format!("&{}", inner)
        }
        ast::Type::ParenType(p) => p
            .ty()
            .map(|t| canonical_type_text(&t))
            .unwrap_or_default(),
        _ => ty.syntax().text().to_string().replace(' ', ""),
    }
}

/// Returns true if `call`'s callee is a path whose last two segments are
/// `Transition::To` (allowing any prefix — `crate::Transition::To`,
/// `mod::Transition::To`, plain `Transition::To` all match).
fn is_transition_to_call(call: &ast::CallExpr) -> bool {
    let Some(ast::Expr::PathExpr(p)) = call.expr() else {
        return false;
    };
    let Some(path) = p.path() else { return false };
    let segments: Vec<String> = path
        .segments()
        .filter_map(|s| s.name_ref().map(|n| n.text().to_string()))
        .collect();
    let n = segments.len();
    n >= 2 && segments[n - 2] == "Transition" && segments[n - 1] == "To"
}

/// Pull the full canonical path text of a variant constructor argument
/// (`Self::Idle`, `MyFsm::Running`, `Idle`). Returns the segments joined by
/// `::`. Rejects non-variant-shaped expressions and any path whose last
/// segment doesn't start with an uppercase letter (function calls, locals).
fn target_path_from_arg_expr(arg: &ast::Expr) -> Option<String> {
    let path = match arg {
        ast::Expr::PathExpr(p) => p.path()?,
        ast::Expr::CallExpr(c) => match c.expr()? {
            ast::Expr::PathExpr(p) => p.path()?,
            _ => return None,
        },
        ast::Expr::RecordExpr(r) => r.path()?,
        _ => return None,
    };
    let segments: Vec<String> = path
        .segments()
        .filter_map(|s| s.name_ref().map(|n| n.text().to_string()))
        .collect();
    let last = segments.last()?;
    if !last.chars().next().is_some_and(|c| c.is_uppercase()) {
        return None;
    }
    Some(segments.join("::"))
}

/// Inspect a function AST node and extract whatever's relevant for transition
/// follow-through: its name, whether it returns `Transition<...>`, and the
/// `Transition::To(...)` targets in its body. Returns `None` if the function
/// has no name.
pub fn analyze_function(func: &ast::Fn) -> Option<(String, FunctionInfo)> {
    use ra_ap_syntax::ast::HasName;
    let name = func.name()?.text().to_string();

    let returns_transition = func
        .ret_type()
        .and_then(|rt| rt.ty())
        .as_ref()
        .is_some_and(type_mentions_transition);

    let mut transition_targets = HashSet::new();
    if let Some(body) = func.body() {
        for descendant in body.syntax().descendants() {
            let Some(call) = ast::CallExpr::cast(descendant) else { continue };
            if !is_transition_to_call(&call) {
                continue;
            }
            let Some(args) = call.arg_list() else { continue };
            let Some(arg) = args.args().next() else { continue };
            if let Some(target) = target_path_from_arg_expr(&arg) {
                transition_targets.insert(target);
            }
        }
    }

    Some((
        name,
        FunctionInfo {
            returns_transition,
            transition_targets,
        },
    ))
}

pub struct TransitionExtractor<'a> {
    pub fsm_name: String,
    pub source_state: String,
    pub current_label: TransitionLabel,
    pub transitions: Vec<TransitionInfo>,
    pub include_guards: bool,
    pub function_registry: &'a FunctionRegistry,
    pub visited_functions: HashSet<String>,
}

impl<'a> TransitionExtractor<'a> {
    pub fn new(
        fsm_name: String,
        source_state: String,
        include_guards: bool,
        function_registry: &'a FunctionRegistry,
    ) -> Self {
        Self {
            fsm_name,
            source_state,
            current_label: TransitionLabel::default(),
            transitions: Vec::new(),
            include_guards,
            function_registry,
            visited_functions: HashSet::new(),
        }
    }

    pub fn extract(&mut self, node: &SyntaxNode) {
        match_ast! {
            match node {
                ast::MatchArm(it) => {
                    let pat = it
                        .pat()
                        .map(|p| p.syntax().text().to_string())
                        .unwrap_or_default();
                    let pat_label = normalize_source(pat);

                    let saved = self.current_label.clone();
                    self.current_label.push_event(pat_label);

                    if self.include_guards {
                        if let Some(guard) = it.guard() {
                            let guard_str =
                                normalize_source(guard.syntax().text().to_string());
                            self.current_label.push_guard(guard_str);
                        }
                    }

                    if let Some(expr) = it.expr() {
                        self.extract(expr.syntax());
                    }

                    self.current_label = saved;
                },
                ast::IfExpr(it) => {
                    let cond = it.condition();

                    // Structural distinction: `if let Pat = expr` (a `LetExpr`)
                    // is treated as an event-match, analogous to a `match` arm
                    // pattern. Any other condition is a guard.
                    let is_let_expr =
                        matches!(cond.as_ref(), Some(ast::Expr::LetExpr(_)));

                    let cond_str = cond
                        .as_ref()
                        .map(|c| normalize_source(c.syntax().text().to_string()))
                        .unwrap_or_default();

                    let saved = self.current_label.clone();

                    if !cond_str.is_empty() && self.include_guards {
                        if is_let_expr {
                            self.current_label.push_event(cond_str);
                        } else {
                            self.current_label.push_guard(cond_str);
                        }
                    }

                    if let Some(block) = it.then_branch() {
                        self.extract(block.syntax());
                    }
                    if let Some(else_branch) = it.else_branch() {
                        match else_branch {
                            ast::ElseBranch::Block(b) => self.extract(b.syntax()),
                            ast::ElseBranch::IfExpr(e) => self.extract(e.syntax()),
                        }
                    }

                    self.current_label = saved;
                },
                ast::CallExpr(it) => {
                    if is_transition_to_call(&it) {
                        if let Some(arg_list) = it.arg_list() {
                            if let Some(arg) = arg_list.args().next() {
                                if let Some(target) =
                                    self.extract_target_state(&arg.syntax())
                                {
                                    self.transitions.push(TransitionInfo {
                                        source: self.source_state.clone(),
                                        target,
                                        label: self.current_label.clone(),
                                    });
                                }
                            }
                        }
                    } else if let Some(expr) = it.expr() {
                        // Free-function call. The follow site re-resolves
                        // via the registry — name only, intentionally.
                        let func_name = expr
                            .syntax()
                            .text()
                            .to_string()
                            .replace(' ', "")
                            .rsplit("::")
                            .next()
                            .unwrap_or("")
                            .to_string();
                        if !func_name.is_empty() {
                            self.follow_function(&func_name);
                        }
                    }
                    for child in node.children() {
                        self.extract(&child);
                    }
                },
                // Method calls used to be followed by name alone — that
                // conflates every `.process()` / `.handle()` across the
                // codebase. Without resolving the receiver's type we can't
                // know which method is meant. Just recurse children.
                _ => {
                    for child in node.children() {
                        self.extract(&child);
                    }
                }
            }
        }
    }

    /// Follow a free-function call into its body, emitting any transitions it
    /// would produce. The follow is *sound* only under three conditions:
    ///
    /// 1. The function name resolves to **exactly one** definition in the
    ///    registry. Same-name collisions across modules return without
    ///    emitting anything — guessing produces phantom edges.
    /// 2. The function's declared return type is `Transition<...>`. Helpers
    ///    that don't return Transition can't drive a state change.
    /// 3. The recorded `Transition::To(...)` target's path prefix matches
    ///    the FSM we're currently extracting for (or is `Self::...`).
    ///    Targets that mention a different FSM are skipped.
    fn follow_function(&mut self, func_name: &str) {
        if !self.visited_functions.insert(func_name.to_string()) {
            return;
        }

        let Some(info) = self.function_registry.lookup_unambiguous(func_name) else {
            return;
        };
        if !info.returns_transition {
            return;
        }

        for target in &info.transition_targets {
            let belongs_to_this_fsm = target.starts_with("Self::")
                || target.starts_with(&format!("{}::", self.fsm_name));
            if !belongs_to_this_fsm {
                continue;
            }

            let state_name = target
                .split("::")
                .last()
                .unwrap_or(target.as_str())
                .to_string();
            if state_name == self.fsm_name || state_name == "Self" || state_name.is_empty() {
                continue;
            }

            let label = if self.current_label.is_empty() {
                TransitionLabel {
                    events: vec![format!("(via {})", func_name)],
                    guards: Vec::new(),
                }
            } else {
                self.current_label.clone()
            };
            self.transitions.push(TransitionInfo {
                source: self.source_state.clone(),
                target: state_name,
                label,
            });
        }
    }

    /// Pull a state name out of the argument expression in
    /// `Transition::To(<arg>)`. Handles the three shapes a state variant can
    /// take:
    ///
    /// - `Self::Idle`               → PathExpr
    /// - `Self::Running(0)`         → CallExpr  (tuple variant constructor)
    /// - `Self::Running { speed }`  → RecordExpr (struct variant constructor)
    ///
    /// Anything else (function calls like `helper()`, method calls like
    /// `self.next()`, control-flow expressions) returns `None` — we don't
    /// emit a phantom edge from a name we can't structurally identify as a
    /// state variant.
    fn extract_target_state(&self, node: &SyntaxNode) -> Option<String> {
        let path = match ast::Expr::cast(node.clone())? {
            ast::Expr::PathExpr(p) => p.path()?,
            ast::Expr::CallExpr(c) => match c.expr()? {
                // The callee of `Self::Running(x)` is the variant path
                // `Self::Running`. A bare `helper()` would land here too,
                // but its last segment starts lowercase and is rejected below.
                ast::Expr::PathExpr(p) => p.path()?,
                _ => return None,
            },
            ast::Expr::RecordExpr(r) => r.path()?,
            _ => return None,
        };

        let last = path.segments().last()?.name_ref()?.text().to_string();

        // Variant identifiers start uppercase by convention; calls/locals
        // start lowercase. Filters out `Transition::To(make_idle())` and
        // similar function-call-shaped arguments.
        if !last.chars().next().is_some_and(|c| c.is_uppercase()) {
            return None;
        }

        if last == self.fsm_name || last == "Self" {
            None
        } else {
            Some(last)
        }
    }
}

pub struct SubFsmExtractor {
    pub fsm_name: String,
    pub discovered: HashSet<String>,
    pub context_fields: HashSet<String>,
}

impl SubFsmExtractor {
    pub fn new(fsm_name: String) -> Self {
        Self {
            fsm_name,
            discovered: HashSet::new(),
            context_fields: HashSet::new(),
        }
    }

    pub fn extract(&mut self, node: &SyntaxNode) {
        for child in node.descendants() {
            match_ast! {
                match child {
                    ast::Path(path) => {
                        // Collect *any* CamelCase path root that isn't the owning
                        // FSM, `Self`, or a known std/library prefix. The decision
                        // of "is this a known FSM?" belongs at the call site
                        // where the registry lives — baking suffix heuristics
                        // (`ends_with("Event")`, `"Fsm"`, etc.) in here causes
                        // false negatives for FSMs that happen to share those
                        // suffixes (e.g. an FSM literally named `OrderState`).
                        //
                        // Only multi-segment paths qualify: a bare `Idle` is
                        // most often a variant pattern, not a type reference.
                        let mut segments = path.segments();
                        if let (Some(first), Some(_)) = (segments.next(), segments.next()) {
                            if let Some(name_ref) = first.name_ref() {
                                let first_str = name_ref.text();
                                let first_char = first_str.chars().next();
                                if first_char.is_some_and(|c| c.is_uppercase())
                                    && first_str != "Self"
                                    && first_str != self.fsm_name.as_str()
                                    && first_str != "Transition"
                                    && first_str != "Option"
                                    && first_str != "Result"
                                    && first_str != "String"
                                {
                                    self.discovered.insert(first_str.to_string());
                                }
                            }
                        }
                    },
                    ast::FieldExpr(it) => {
                        if let Some(name) = it.name_ref() {
                            self.context_fields.insert(name.text().to_string());
                        }
                    },
                    _ => {}
                }
            }
        }
    }
}

/// Tiny token-by-token cursor over a flat slice. Encapsulates the manual
/// `i += 1`, bounds-check and `tokens[i].kind()` patterns that infected
/// `parse_macro_body` — making the surrounding control flow much easier to
/// audit for off-by-ones and missed advances.
struct Cursor<'a> {
    tokens: &'a [SyntaxToken],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(tokens: &'a [SyntaxToken]) -> Self {
        Self { tokens, pos: 0 }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.tokens.len()
    }

    fn peek(&self) -> Option<&'a SyntaxToken> {
        self.tokens.get(self.pos)
    }

    fn peek_kind(&self) -> Option<SyntaxKind> {
        self.peek().map(|t| t.kind())
    }

    fn peek_text(&self) -> Option<&'a str> {
        self.peek().map(|t| t.text())
    }

    fn peek_kind_at(&self, offset: usize) -> Option<SyntaxKind> {
        self.tokens.get(self.pos + offset).map(|t| t.kind())
    }

    fn advance(&mut self) {
        if self.pos < self.tokens.len() {
            self.pos += 1;
        }
    }

    /// Consume the next token if it has `kind`; return whether we did.
    fn eat(&mut self, kind: SyntaxKind) -> bool {
        if self.peek_kind() == Some(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Consume `=>`, tolerating both the single-punct form and the `=` `>`
    /// pair some token streams produce.
    fn eat_fat_arrow(&mut self) -> bool {
        if self.eat(T![=>]) {
            return true;
        }
        if self.peek_kind() == Some(T![=]) && self.peek_kind_at(1) == Some(T![>]) {
            self.advance();
            self.advance();
            return true;
        }
        false
    }
}

/// Join `tokens` back into a single source-text string, using the same
/// spacing rule the original cursor-based parser used: a single space between
/// adjacent non-punct tokens. The result is whitespace-stripped (no leading,
/// trailing, or multiple spaces) and re-parseable by ra_ap_syntax.
fn reconstruct_tokens(tokens: &[SyntaxToken]) -> String {
    let mut out = String::new();
    for (i, tok) in tokens.iter().enumerate() {
        out.push_str(tok.text());
        if i + 1 < tokens.len()
            && !tokens[i + 1].kind().is_punct()
            && !tok.kind().is_punct()
        {
            out.push(' ');
        }
    }
    out
}

/// Consume a balanced `{ … }` span from the cursor and return the
/// `(start, end_exclusive)` token indices that cover it. Cursor ends up just
/// past the closing brace on success. Returns `None` if the next token isn't
/// `{` or if the braces are unbalanced.
fn consume_balanced_braces(c: &mut Cursor) -> Option<(usize, usize)> {
    if c.peek_kind() != Some(T!['{']) {
        return None;
    }
    let start = c.pos;
    let mut depth: i32 = 0;
    while !c.at_end() {
        match c.peek_kind() {
            Some(T!['{']) => depth += 1,
            Some(T!['}']) => {
                depth -= 1;
                c.advance();
                if depth == 0 {
                    return Some((start, c.pos));
                }
                continue;
            }
            _ => {}
        }
        c.advance();
    }
    None
}

/// Lifecycle blocks extracted from a single state's body.
#[derive(Default)]
struct StateHooks {
    entry: Option<String>,
    process: Option<String>,
    exit: Option<String>,
}

/// Structurally parse a state body `{ entry: …, process: …, exit: … }` by
/// wrapping its tokens in a synthetic struct-literal expression, handing it
/// to ra_ap_syntax, and walking the resulting `RecordExpr`. Each closure
/// hook is returned as its full source text (`|args| { body }`).
///
/// Replaces the old `j`-index lifecycle loop. The wrap is `let _ = _S { … };`
/// inside a dummy `fn _f() { … }` so the macro body's struct-literal-shaped
/// syntax becomes a real `ast::RecordExpr` we can walk.
fn parse_state_body_hooks(tokens: &[SyntaxToken]) -> StateHooks {
    let mut hooks = StateHooks::default();
    let body_text = reconstruct_tokens(tokens);
    let wrapped = format!("fn _f() {{ let _ = _S {}; }}", body_text);
    let parse = SourceFile::parse(&wrapped, Edition::Edition2021);
    let Some(record) = parse
        .tree()
        .syntax()
        .descendants()
        .find_map(ast::RecordExpr::cast)
    else {
        return hooks;
    };
    let Some(field_list) = record.record_expr_field_list() else {
        return hooks;
    };
    for field in field_list.fields() {
        let Some(name_ref) = field.field_name() else { continue };
        let Some(expr) = field.expr() else { continue };
        let value_text = expr.syntax().text().to_string();
        match name_ref.text().as_str() {
            "entry" => hooks.entry = Some(value_text),
            "process" => hooks.process = Some(value_text),
            "exit" => hooks.exit = Some(value_text),
            _ => {}
        }
    }
    hooks
}

/// Structurally parse a state's `{ field: type, … }` field list by wrapping
/// its tokens in a synthetic struct declaration and walking the resulting
/// `RecordFieldList`. Field types pass through `canonical_type_text` so
/// downstream alias and registry lookups see the same canonical form.
fn parse_state_fields(tokens: &[SyntaxToken]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let fields_text = reconstruct_tokens(tokens);
    let wrapped = format!("struct _S {};", fields_text);
    let parse = SourceFile::parse(&wrapped, Edition::Edition2021);
    let Some(s) = parse
        .tree()
        .syntax()
        .descendants()
        .find_map(ast::Struct::cast)
    else {
        return out;
    };
    let Some(ast::FieldList::RecordFieldList(list)) = s.field_list() else {
        return out;
    };
    for field in list.fields() {
        let Some(name) = field.name() else { continue };
        let Some(ty) = field.ty() else { continue };
        out.push((name.text().to_string(), canonical_type_text(&ty)));
    }
    out
}

pub fn parse_macro_body(token_tree: ast::TokenTree) -> Result<FsmDefinition, ParseError> {
    let mut name: Option<String> = None;
    let mut context_type = None;
    let mut states = Vec::new();
    let mut first_state_error: Option<ParseError> = None;

    let tokens: Vec<SyntaxToken> = token_tree
        .syntax()
        .descendants_with_tokens()
        .filter_map(|it| it.into_token())
        .filter(|t| t.kind() != SyntaxKind::WHITESPACE && t.kind() != SyntaxKind::COMMENT)
        .collect();

    let mut c = Cursor::new(&tokens);
    c.eat(T!['{']);

    while !c.at_end() && c.peek_kind() != Some(T!['}']) {
        let Some(text) = c.peek_text() else { break };

        if text == "Name" {
            c.advance();
            if c.eat(T![:]) {
                if let Some(tok) = c.peek() {
                    name = Some(tok.text().to_string());
                    c.advance();
                }
            }
        } else if text == "Context" {
            c.advance();
            if c.eat(T![:]) {
                let mut ty = String::new();
                while !c.at_end()
                    && c.peek_kind() != Some(T![,])
                    && c.peek_text() != Some("Event")
                    && c.peek_text() != Some("States")
                {
                    ty.push_str(c.peek_text().unwrap_or(""));
                    c.advance();
                }
                context_type = Some(ty);
            }
        } else if text == "States" {
            c.advance();
            if c.eat(T![:]) && c.eat(T!['{']) {
                while !c.at_end() && c.peek_kind() != Some(T!['}']) {
                    let state_name = c.peek_text().unwrap_or("").to_string();
                    c.advance();

                    // Optional `{ field: type, … }` field list — structural
                    // parse via a synthetic `struct _S { … };` wrapper.
                    let fields = if let Some((fs, fe)) = consume_balanced_braces(&mut c) {
                        parse_state_fields(&tokens[fs..fe])
                    } else {
                        Vec::new()
                    };

                    c.eat_fat_arrow();

                    // State body `{ entry: …, process: …, exit: … }` —
                    // structural parse via a synthetic `let _ = _S { … };`
                    // wrapper.
                    if let Some((bs, be)) = consume_balanced_braces(&mut c) {
                        let hooks = parse_state_body_hooks(&tokens[bs..be]);
                        if let Some(process_block) = hooks.process {
                            states.push(StateDefinition {
                                name: state_name,
                                fields,
                                entry_block: hooks.entry,
                                process_block,
                                exit_block: hooks.exit,
                            });
                        } else if first_state_error.is_none() {
                            first_state_error =
                                Some(ParseError::StateMissingProcessBlock(state_name));
                        }
                    }

                    c.eat(T![,]);
                }
                c.eat(T!['}']);
            }
        } else {
            // Unknown token at the top level — eat it (covers stray `,`,
            // unfamiliar keys we don't know about yet, etc.).
            c.advance();
        }
    }

    let name = name.ok_or(ParseError::MissingName)?;

    if states.is_empty() {
        // Prefer a per-state error so the user sees *which* state is broken
        // rather than just "no states".
        return Err(first_state_error.unwrap_or(ParseError::EmptyStates));
    }

    Ok(FsmDefinition {
        name,
        context_type,
        states,
    })
}

pub fn generate_mermaid_simple(
    fsm: &FsmDefinition,
    include_guards: bool,
    function_registry: &FunctionRegistry,
) -> Result<String, DiagramError> {
    let mut builder = StateDiagramBuilder::default();
    let mut nodes = HashMap::new();
    let mut all_edges = Vec::new();

    for state in &fsm.states {
        let state_name = state.name.clone();
        let node_builder = StateNodeBuilder::default()
            .label(&state_name)
            .map_err(build_err)?;
        let node = builder.node(node_builder).map_err(build_err)?;
        nodes.insert(state_name.clone(), node);

        let mut extractor = TransitionExtractor::new(
            fsm.name.clone(),
            state_name,
            include_guards,
            function_registry,
        );
        let parse = SourceFile::parse(&state.process_block, Edition::Edition2021);
        extractor.extract(&parse.tree().syntax());

        for trans in extractor.transitions {
            all_edges.push(trans);
        }
    }

    for trans in all_edges {
        if let (Some(src), Some(dst)) = (nodes.get(&trans.source), nodes.get(&trans.target)) {
            let label = format_label_for_mermaid(&trans.label);
            let edge = StateEdgeBuilder::default()
                .source(src.clone())
                .map_err(build_err)?
                .destination(dst.clone())
                .map_err(build_err)?
                .label(&label)
                .map_err(build_err)?;
            builder.edge(edge).map_err(build_err)?;
        }
    }

    Ok(StateDiagram::from(builder)
        .to_string()
        .replace("\r\n", "\n")
        .replace("    direction LR\n", ""))
}

/// Collect every FSM-like name referenced by a single state — both via
/// explicit type paths in its blocks and via the FSM's context-struct fields
/// that the state's code touches. The result is NOT filtered by any known-FSM
/// registry; callers do that themselves (e.g. by intersecting with
/// `all_fsms.contains_key(...)`).
pub fn collect_referenced_fsms_in_state<F>(
    fsm_name: &str,
    fsm_context_type: Option<&str>,
    state: &StateDefinition,
    context_struct_map: &HashMap<String, HashMap<String, String>>,
    resolve_type: F,
) -> HashSet<String>
where
    F: Fn(&str) -> String + Copy,
{
    let mut extractor = SubFsmExtractor::new(fsm_name.to_string());

    if let Some(entry) = &state.entry_block {
        let parse = SourceFile::parse(entry, Edition::Edition2021);
        extractor.extract(&parse.tree().syntax());
    }
    let parse = SourceFile::parse(&state.process_block, Edition::Edition2021);
    extractor.extract(&parse.tree().syntax());
    if let Some(exit) = &state.exit_block {
        let parse = SourceFile::parse(exit, Edition::Edition2021);
        extractor.extract(&parse.tree().syntax());
    }
    for (_, f_type) in &state.fields {
        let parse = SourceFile::parse(f_type, Edition::Edition2021);
        extractor.extract(&parse.tree().syntax());
    }

    let mut found = HashSet::new();

    // Explicit: any CamelCase path roots inside the macro body, resolved
    // through type aliases. Stored as-resolved — caller may want to split
    // on `::` to match against an unqualified FSM registry.
    for child in &extractor.discovered {
        found.insert(resolve_type(child));
    }

    // Contextual: types of context-struct fields the FSM's code touches.
    // Split on `::` so we match against base FSM names.
    if let Some(ctx_type) = fsm_context_type {
        let ctx_name = resolve_type(&ctx_type.replace(' ', ""));
        if let Some(fields) = context_struct_map.get(&ctx_name) {
            for field_name in &extractor.context_fields {
                if let Some(type_name) = fields.get(field_name) {
                    let resolved_type = resolve_type(type_name);
                    let base_type = resolved_type
                        .split("::")
                        .last()
                        .unwrap_or(&resolved_type)
                        .to_string();
                    found.insert(base_type);
                }
            }
        }
    }

    found
}

/// Collect every FSM-like name referenced anywhere across all states of `fsm`.
/// Convenience aggregator on top of [`collect_referenced_fsms_in_state`].
pub fn collect_referenced_fsms<F>(
    fsm: &FsmDefinition,
    context_struct_map: &HashMap<String, HashMap<String, String>>,
    resolve_type: F,
) -> HashSet<String>
where
    F: Fn(&str) -> String + Copy,
{
    let mut found = HashSet::new();
    for state in &fsm.states {
        found.extend(collect_referenced_fsms_in_state(
            &fsm.name,
            fsm.context_type.as_deref(),
            state,
            context_struct_map,
            resolve_type,
        ));
    }
    found
}

fn populate_builder_hierarchical<F>(
    fsm: &FsmDefinition,
    all_fsms: &HashMap<String, &FsmDefinition>,
    context_struct_map: &HashMap<String, HashMap<String, String>>,
    include_guards: bool,
    function_registry: &FunctionRegistry,
    resolve_type: F,
) -> Result<StateDiagramBuilder, DiagramError>
where
    F: Fn(&str) -> String + Copy,
{
    let mut builder = StateDiagramBuilder::default();
    let mut nodes = HashMap::new();
    let mut all_edges = Vec::new();

    for state in &fsm.states {
        let state_name = state.name.clone();
        let mut node_builder = StateNodeBuilder::default()
            .label(&state_name)
            .map_err(build_err)?;

        let referenced = collect_referenced_fsms_in_state(
            &fsm.name,
            fsm.context_type.as_deref(),
            state,
            context_struct_map,
            resolve_type,
        );

        for sub_name in referenced {
            if let Some(sub_fsm) = all_fsms.get(&sub_name) {
                let sub_builder = populate_builder_hierarchical(
                    sub_fsm,
                    all_fsms,
                    context_struct_map,
                    include_guards,
                    function_registry,
                    resolve_type,
                )?;
                node_builder = node_builder
                    .inner_diagram(StateDiagram::from(sub_builder))
                    .map_err(build_err)?;
            }
        }

        let node = builder.node(node_builder).map_err(build_err)?;
        nodes.insert(state_name.clone(), node);

        let mut trans_extractor = TransitionExtractor::new(
            fsm.name.clone(),
            state_name,
            include_guards,
            function_registry,
        );
        let parse = SourceFile::parse(&state.process_block, Edition::Edition2021);
        trans_extractor.extract(&parse.tree().syntax());

        for trans in trans_extractor.transitions {
            all_edges.push(trans);
        }
    }

    for trans in all_edges {
        if let (Some(src), Some(dst)) = (nodes.get(&trans.source), nodes.get(&trans.target)) {
            let label = format_label_for_mermaid(&trans.label);
            let edge = StateEdgeBuilder::default()
                .source(src.clone())
                .map_err(build_err)?
                .destination(dst.clone())
                .map_err(build_err)?
                .label(&label)
                .map_err(build_err)?;
            builder.edge(edge).map_err(build_err)?;
        }
    }

    Ok(builder)
}

pub fn generate_mermaid_hierarchical<F>(
    fsm: &FsmDefinition,
    all_fsms: &HashMap<String, &FsmDefinition>,
    context_struct_map: &HashMap<String, HashMap<String, String>>,
    include_guards: bool,
    function_registry: &FunctionRegistry,
    resolve_type: F,
) -> Result<String, DiagramError>
where
    F: Fn(&str) -> String + Copy,
{
    let builder = populate_builder_hierarchical(
        fsm,
        all_fsms,
        context_struct_map,
        include_guards,
        function_registry,
        resolve_type,
    )?;
    Ok(StateDiagram::from(builder)
        .to_string()
        .replace("\r\n", "\n")
        .replace("    direction LR\n", ""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_source() {
        // Source normalization MUST NOT leak Mermaid-presentation tokens
        // (`<br/>`, `#colon;`) — those belong to format_label_for_mermaid.
        let input = "ctx . timer . is_expired ( )".to_string();
        assert_eq!(normalize_source(input), "ctx.timer.is_expired()");

        let input = "ButtonEvent :: Press".to_string();
        assert_eq!(normalize_source(input), "ButtonEvent::Press");

        let input = "if ! ctx . is_active ( )".to_string();
        assert_eq!(normalize_source(input), "if !ctx.is_active()");

        let input = "ctx.tick_count>=3".to_string();
        assert_eq!(normalize_source(input), "ctx.tick_count >= 3");

        let input = "a && b || c".to_string();
        assert_eq!(normalize_source(input), "a and b or c");

        let input = "HeaderScheduleEvent :: Apply { resweep , force_occupancy_broadcast , occupancy_changed , }".to_string();
        assert_eq!(
            normalize_source(input),
            "HeaderScheduleEvent::Apply { resweep, force_occupancy_broadcast, occupancy_changed }",
        );

        let input = "Event :: A | Event :: B".to_string();
        assert_eq!(normalize_source(input), "Event::A | Event::B");

        let input = "Apply{a,b,}".to_string();
        assert_eq!(normalize_source(input), "Apply { a, b }");

        let input =
            "HeaderScheduleEvent::Apply{resweep,force_occupancy_broadcast,occupancy_changed,}"
                .to_string();
        assert_eq!(
            normalize_source(input),
            "HeaderScheduleEvent::Apply { resweep, force_occupancy_broadcast, occupancy_changed }",
        );
    }

    #[test]
    fn format_label_empty_renders_star() {
        let label = TransitionLabel::default();
        assert_eq!(format_label_for_mermaid(&label), "*");
    }

    #[test]
    fn format_label_event_only_escapes_colons() {
        let label = TransitionLabel {
            events: vec!["Event::Press".to_string()],
            guards: vec![],
        };
        assert_eq!(format_label_for_mermaid(&label), "Event#colon;#colon;Press");
    }

    #[test]
    fn format_label_with_single_guard() {
        let label = TransitionLabel {
            events: vec!["Press".to_string()],
            guards: vec!["a > 0".to_string()],
        };
        assert_eq!(format_label_for_mermaid(&label), "Press<br/>[ if a > 0 ]");
    }

    #[test]
    fn format_label_multiple_guards_joined_with_and() {
        let label = TransitionLabel {
            events: vec!["Press".to_string()],
            guards: vec!["a > 0".to_string(), "b < 5".to_string()],
        };
        assert_eq!(
            format_label_for_mermaid(&label),
            "Press<br/>[ if a > 0 and b < 5 ]",
        );
    }

    #[test]
    fn format_label_inserts_br_before_destructured_brace() {
        let label = TransitionLabel {
            events: vec!["Apply { a, b }".to_string()],
            guards: vec![],
        };
        assert_eq!(format_label_for_mermaid(&label), "Apply<br/>{ a, b }");
    }

    #[test]
    fn format_label_guard_only_no_event_part() {
        let label = TransitionLabel {
            events: vec![],
            guards: vec!["x == 1".to_string()],
        };
        assert_eq!(format_label_for_mermaid(&label), "[ if x == 1 ]");
    }

    /// Parse `state_machine! { … }` source and run it through `parse_macro_body`.
    fn parse_for_test(src: &str) -> Result<FsmDefinition, ParseError> {
        let parse = SourceFile::parse(src, Edition::Edition2021);
        let token_tree = parse
            .tree()
            .syntax()
            .descendants()
            .find_map(ast::MacroCall::cast)
            .and_then(|m| m.token_tree())
            .expect("test fixture must contain a state_machine! invocation");
        parse_macro_body(token_tree)
    }

    #[test]
    fn parse_macro_body_happy_path() {
        let src = r#"
            state_machine! {
                Name: MiniFsm,
                Context: MiniCtx,
                States: {
                    Idle => {
                        process: |_ctx, _evt| { Transition::None }
                    },
                    Running { speed: u32 } => {
                        entry: |_ctx| {},
                        process: |_ctx, _evt| { Transition::To(Self::Idle) },
                        exit: |_ctx| {}
                    }
                }
            }
        "#;
        let fsm = parse_for_test(src).expect("valid macro should parse");
        assert_eq!(fsm.name, "MiniFsm");
        assert_eq!(
            fsm.states
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Idle", "Running"],
        );
        assert!(fsm.states[1].entry_block.is_some());
        assert!(fsm.states[1].exit_block.is_some());
        // Running has one field
        assert_eq!(fsm.states[1].fields.len(), 1);
        assert_eq!(fsm.states[1].fields[0].0, "speed");
    }

    #[test]
    fn parse_macro_body_missing_name_errors() {
        let src = r#"
            state_machine! {
                States: {
                    Idle => { process: |_ctx, _evt| { Transition::None } }
                }
            }
        "#;
        assert_eq!(parse_for_test(src).unwrap_err(), ParseError::MissingName);
    }

    #[test]
    fn parse_macro_body_empty_states_errors() {
        // No States: block at all.
        let src = r#"
            state_machine! {
                Name: NoStates,
                Context: SomeCtx
            }
        "#;
        assert_eq!(parse_for_test(src).unwrap_err(), ParseError::EmptyStates);

        // Empty States: block.
        let src = r#"
            state_machine! {
                Name: EmptyStates,
                States: {}
            }
        "#;
        assert_eq!(parse_for_test(src).unwrap_err(), ParseError::EmptyStates);
    }

    #[test]
    fn parse_macro_body_state_missing_process_block_errors() {
        // `Broken` has only an `entry:` block, no `process:`.
        let src = r#"
            state_machine! {
                Name: Partial,
                States: {
                    Broken => {
                        entry: |_ctx| {}
                    }
                }
            }
        "#;
        assert_eq!(
            parse_for_test(src).unwrap_err(),
            ParseError::StateMissingProcessBlock("Broken".to_string()),
        );
    }

    /// Parse Rust source and return the first function definition found.
    fn parse_first_fn(src: &str) -> ast::Fn {
        let parse = SourceFile::parse(src, Edition::Edition2021);
        parse
            .tree()
            .syntax()
            .descendants()
            .find_map(ast::Fn::cast)
            .expect("test fixture must contain a function")
    }

    #[test]
    fn analyze_function_records_transition_targets() {
        let src = r#"
            fn handle(ctx: &mut Ctx) -> Transition<MyFsm> {
                if ctx.flag {
                    Transition::To(Self::Ready)
                } else {
                    Transition::To(MyFsm::Idle)
                }
            }
        "#;
        let f = parse_first_fn(src);
        let (name, info) = analyze_function(&f).expect("function should analyze");
        assert_eq!(name, "handle");
        assert!(info.returns_transition);
        assert_eq!(
            info.transition_targets,
            ["Self::Ready".to_string(), "MyFsm::Idle".to_string()]
                .into_iter()
                .collect::<HashSet<_>>(),
        );
    }

    /// Parse a `let x: Ty = …;` and return the `Ty` AST node — handy for
    /// canonical_type_text tests since `ast::Type` doesn't have a top-level
    /// parser entry point.
    fn parse_type(ty_src: &str) -> ast::Type {
        let src = format!("fn _f() {{ let _x: {} = todo!(); }}", ty_src);
        let parse = SourceFile::parse(&src, Edition::Edition2021);
        parse
            .tree()
            .syntax()
            .descendants()
            .find_map(ast::LetStmt::cast)
            .and_then(|s| s.ty())
            .expect("test fixture must declare a typed local")
    }

    #[test]
    fn canonical_type_text_strips_generics() {
        // Generics on the path segment are dropped — what we want as a
        // registry key is the *base* type, since downstream takes the last
        // `::` segment.
        assert_eq!(canonical_type_text(&parse_type("Vec<MyFsm>")), "Vec");
        assert_eq!(
            canonical_type_text(&parse_type("Result<Transition<Self>, Err>")),
            "Result",
        );
    }

    #[test]
    fn canonical_type_text_preserves_path_segments() {
        assert_eq!(canonical_type_text(&parse_type("Foo::Bar::Baz")), "Foo::Bar::Baz");
        assert_eq!(canonical_type_text(&parse_type("MyFsm")), "MyFsm");
    }

    #[test]
    fn canonical_type_text_preserves_reference_marker() {
        // Refs aren't dereferenced — the marker has to survive so an alias
        // written `type Foo = &MyFsm;` still keys the same way.
        assert_eq!(canonical_type_text(&parse_type("&MyFsm")), "&MyFsm");
    }

    #[test]
    fn canonical_type_text_unwraps_parentheses() {
        assert_eq!(canonical_type_text(&parse_type("(MyFsm)")), "MyFsm");
    }

    #[test]
    fn analyze_function_does_not_match_substring_named_types() {
        // The old substring check fired on `MyTransition` too — fixed by
        // segment-name matching.
        let src = r#"
            fn helper(ctx: &Ctx) -> MyTransition { todo!() }
        "#;
        let f = parse_first_fn(src);
        let (_, info) = analyze_function(&f).expect("should analyze");
        assert!(
            !info.returns_transition,
            "MyTransition should not be classified as Transition",
        );
    }

    #[test]
    fn analyze_function_finds_transition_inside_generic_args() {
        // `Result<Transition<Self>, Err>` — the outer type isn't Transition
        // but its generic arg is.
        let src = r#"
            fn try_step() -> Result<Transition<Self>, MyErr> { todo!() }
        "#;
        let f = parse_first_fn(src);
        let (_, info) = analyze_function(&f).expect("should analyze");
        assert!(info.returns_transition);
    }

    #[test]
    fn analyze_function_records_full_target_path() {
        // The follow site checks the prefix (Self::… vs OtherFsm::…), so the
        // recorded target must keep that prefix — not just the last segment.
        let src = r#"
            fn handle(ctx: &Ctx) -> Transition<MyFsm> {
                Transition::To(MyFsm::Running { speed: 0 })
            }
        "#;
        let f = parse_first_fn(src);
        let (_, info) = analyze_function(&f).expect("should analyze");
        assert!(info.transition_targets.contains(&"MyFsm::Running".to_string()));
    }

    #[test]
    fn analyze_function_skips_helpers_not_returning_transition() {
        let src = r#"
            fn helper(ctx: &mut Ctx) -> u32 {
                Transition::To(Self::Ready);
                42
            }
        "#;
        let f = parse_first_fn(src);
        let (_, info) = analyze_function(&f).expect("function should analyze");
        // Body still mentions Transition::To, but returns_transition gates
        // whether the follow-site will use it at all.
        assert!(!info.returns_transition);
        // We still record the targets — the gate is on the consumer side, not here.
        assert!(info
            .transition_targets
            .contains(&"Self::Ready".to_string()));
    }

    #[test]
    fn function_registry_returns_none_on_collision() {
        let mut reg = FunctionRegistry::new();
        reg.record(
            "shared_name".to_string(),
            FunctionInfo {
                returns_transition: true,
                transition_targets: ["Self::A".to_string()].into_iter().collect(),
            },
        );
        reg.record(
            "shared_name".to_string(),
            FunctionInfo {
                returns_transition: true,
                transition_targets: ["Self::B".to_string()].into_iter().collect(),
            },
        );
        // Two definitions with the same short name — the registry refuses to
        // pick one. The follow site will skip rather than emit phantom edges.
        assert!(reg.lookup_unambiguous("shared_name").is_none());
        assert!(reg.lookup_unambiguous("never_seen").is_none());
    }

    #[test]
    fn function_registry_resolves_unique_name() {
        let mut reg = FunctionRegistry::new();
        reg.record(
            "unique".to_string(),
            FunctionInfo {
                returns_transition: true,
                transition_targets: ["Self::A".to_string()].into_iter().collect(),
            },
        );
        let info = reg.lookup_unambiguous("unique").expect("should resolve");
        assert!(info.returns_transition);
        assert!(info.transition_targets.contains(&"Self::A".to_string()));
    }

    #[test]
    fn parse_macro_body_handles_closure_with_pipe_in_body() {
        // Match alternation `|` inside the closure body must NOT confuse the
        // pipe-counter that delimits the closure's arg list. The body's `|`s
        // are inside the `{...}` block, so `inner_depth > 0` short-circuits
        // the break check entirely.
        let src = r#"
            state_machine! {
                Name: AltFsm,
                States: {
                    Active => {
                        process: |_ctx, evt| {
                            match evt {
                                Event::A | Event::B => Transition::To(Self::Done),
                                _ => Transition::None,
                            }
                        }
                    },
                    Done => {
                        process: |_ctx, _evt| { Transition::None }
                    }
                }
            }
        "#;
        let fsm = parse_for_test(src).expect("alternation in pattern should parse");
        assert_eq!(fsm.name, "AltFsm");
        assert_eq!(fsm.states.len(), 2);
    }

    #[test]
    fn parse_macro_body_handles_multiple_lifecycle_hooks_per_state() {
        // entry, process, exit all present + a comma between them + another
        // state after — the lifecycle parser must terminate each block at the
        // right point.
        let src = r#"
            state_machine! {
                Name: TriHook,
                States: {
                    Loaded => {
                        entry: |_ctx| { let _x = 1; },
                        process: |_ctx, _evt| { Transition::To(Self::Empty) },
                        exit: |_ctx| { let _y = 2; }
                    },
                    Empty => {
                        process: |_ctx, _evt| { Transition::None }
                    }
                }
            }
        "#;
        let fsm = parse_for_test(src).expect("multi-hook state should parse");
        let loaded = &fsm.states[0];
        assert_eq!(loaded.name, "Loaded");
        assert!(loaded.entry_block.is_some());
        assert!(loaded.exit_block.is_some());
        assert_eq!(fsm.states[1].name, "Empty");
    }

    /// Build a TransitionExtractor and feed it a `process` body fragment.
    /// Returns the list of (target_state, label_events) pairs it produced.
    fn extract_transitions(
        fsm_name: &str,
        source_state: &str,
        body: &str,
    ) -> Vec<(String, Vec<String>)> {
        let registry = FunctionRegistry::new();
        let mut ex =
            TransitionExtractor::new(fsm_name.to_string(), source_state.to_string(), true, &registry);
        let parse = SourceFile::parse(body, Edition::Edition2021);
        ex.extract(&parse.tree().syntax());
        ex.transitions
            .into_iter()
            .map(|t| (t.target, t.label.events))
            .collect()
    }

    #[test]
    fn extract_target_state_handles_all_three_variant_shapes() {
        // Path: Self::A, Call: Self::B(0), Record: Self::C { x: 0 }
        let body = r#"
            fn _f() {
                Transition::To(Self::A);
                Transition::To(Self::B(0));
                Transition::To(Self::C { x: 0 });
            }
        "#;
        let mut targets: Vec<String> =
            extract_transitions("Fsm", "Start", body).into_iter().map(|(t, _)| t).collect();
        targets.sort();
        assert_eq!(targets, vec!["A".to_string(), "B".to_string(), "C".to_string()]);
    }

    #[test]
    fn extract_target_state_rejects_function_call_arguments() {
        // `Transition::To(make_idle())` — `make_idle` starts lowercase so the
        // structural check correctly refuses to emit "make_idle" as a state.
        // (The function-following pass handles indirect transitions instead.)
        let body = r#"
            fn _f() {
                Transition::To(make_idle());
            }
        "#;
        let targets = extract_transitions("Fsm", "Start", body);
        assert!(
            targets.is_empty(),
            "expected no direct target from a function-call arg, got {:?}",
            targets,
        );
    }

    #[test]
    fn extract_target_state_rejects_method_call_argument() {
        // `Transition::To(self.next())` — MethodCallExpr isn't a variant
        // constructor; emit nothing rather than guessing.
        let body = r#"
            fn _f() {
                Transition::To(self.next());
            }
        "#;
        let targets = extract_transitions("Fsm", "Start", body);
        assert!(targets.is_empty(), "method-call arg should not become a target");
    }

    #[test]
    fn parse_macro_body_handles_no_arg_closure() {
        // `|| { body }` — empty args. The pipe-counter version could mis-count
        // these because the two `|`s are adjacent.
        let src = r#"
            state_machine! {
                Name: NoArgs,
                States: {
                    Only => {
                        entry: || { let _z = 0; },
                        process: |_c, _e| { Transition::None }
                    }
                }
            }
        "#;
        let fsm = parse_for_test(src).expect("no-arg closure should parse");
        assert!(fsm.states[0].entry_block.is_some());
    }

    #[test]
    fn parse_macro_body_handles_bitwise_or_in_closure_body() {
        // `flags | OTHER_FLAG` inside the body must not affect closure
        // boundaries — the body is inside `{...}` so depth tracking covers it.
        let src = r#"
            state_machine! {
                Name: BitOr,
                States: {
                    Active => {
                        process: |ctx, _evt| {
                            let _mask = ctx.flags | 0b1010;
                            Transition::None
                        }
                    }
                }
            }
        "#;
        let fsm = parse_for_test(src).expect("bitwise-or in body should parse");
        assert_eq!(fsm.states[0].name, "Active");
        // The process block text should contain the body's bitwise-or expression.
        assert!(fsm.states[0].process_block.contains("flags"));
    }

    #[test]
    fn parse_macro_body_handles_state_with_no_fields() {
        // No `{ field: ty }` between state name and `=>`.
        let src = r#"
            state_machine! {
                Name: NoFields,
                States: {
                    Alpha => { process: |_c, _e| { Transition::None } },
                    Beta => { process: |_c, _e| { Transition::None } }
                }
            }
        "#;
        let fsm = parse_for_test(src).expect("fieldless states should parse");
        assert_eq!(fsm.states.len(), 2);
        assert!(fsm.states.iter().all(|s| s.fields.is_empty()));
    }

    #[test]
    fn parse_macro_body_reports_first_broken_state_when_others_also_broken() {
        // If multiple states are malformed, surface the FIRST one — enough
        // signal for the user to start fixing without overwhelming them.
        let src = r#"
            state_machine! {
                Name: Multi,
                States: {
                    First => {
                        entry: |_ctx| {}
                    },
                    Second => {
                        exit: |_ctx| {}
                    }
                }
            }
        "#;
        assert_eq!(
            parse_for_test(src).unwrap_err(),
            ParseError::StateMissingProcessBlock("First".to_string()),
        );
    }
}
