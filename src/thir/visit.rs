use super::*;

pub trait ThirVisitor {
    fn visit_block(&mut self, block: &TBlock) {
        walk_block(self, block);
    }

    fn visit_stmt(&mut self, stmt: &TStmt) {
        walk_stmt(self, stmt);
    }

    fn visit_for_init(&mut self, init: &TForInit) {
        walk_for_init(self, init);
    }

    fn visit_case(&mut self, case: &TCase) {
        walk_case(self, case);
    }

    fn visit_pattern(&mut self, pattern: &TPattern) {
        walk_pattern(self, pattern);
    }

    fn visit_expr(&mut self, expr: &TExpr) {
        walk_expr(self, expr);
    }

    fn visit_closure_body(&mut self, body: &TClosureBody) {
        walk_closure_body(self, body);
    }
}

pub fn walk_block<V: ThirVisitor + ?Sized>(visitor: &mut V, block: &TBlock) {
    for stmt in &block.statements {
        visitor.visit_stmt(stmt);
    }
}

pub fn walk_stmt<V: ThirVisitor + ?Sized>(visitor: &mut V, stmt: &TStmt) {
    match &stmt.kind {
        TStmtKind::Block(block) => visitor.visit_block(block),
        TStmtKind::VarDecl { init, .. } => {
            if let Some(init) = init {
                visitor.visit_expr(init);
            }
        }
        TStmtKind::Assign { target, value } => {
            visitor.visit_expr(target);
            visitor.visit_expr(value);
        }
        TStmtKind::If {
            cond,
            then_block,
            else_branch,
        } => {
            visitor.visit_expr(cond);
            visitor.visit_block(then_block);
            if let Some(else_branch) = else_branch {
                visitor.visit_stmt(else_branch);
            }
        }
        TStmtKind::While { cond, body } => {
            visitor.visit_expr(cond);
            visitor.visit_block(body);
        }
        TStmtKind::For {
            init,
            cond,
            step,
            body,
        } => {
            if let Some(init) = init {
                visitor.visit_for_init(init);
            }
            if let Some(cond) = cond {
                visitor.visit_expr(cond);
            }
            if let Some(step) = step {
                visitor.visit_for_init(step);
            }
            visitor.visit_block(body);
        }
        TStmtKind::Switch {
            expr,
            cases,
            default,
            ..
        } => {
            visitor.visit_expr(expr);
            for case in cases {
                visitor.visit_case(case);
            }
            for stmt in default {
                visitor.visit_stmt(stmt);
            }
        }
        TStmtKind::Defer(expr)
        | TStmtKind::ResourceCleanup(expr)
        | TStmtKind::Return(Some(expr))
        | TStmtKind::Expr(expr) => {
            visitor.visit_expr(expr);
        }
        TStmtKind::Return(None)
        | TStmtKind::Break
        | TStmtKind::Continue
        | TStmtKind::Unsupported => {}
    }
}

pub fn walk_for_init<V: ThirVisitor + ?Sized>(visitor: &mut V, init: &TForInit) {
    match init {
        TForInit::VarDecl { init, .. } => {
            if let Some(init) = init {
                visitor.visit_expr(init);
            }
        }
        TForInit::Assign { target, value } => {
            visitor.visit_expr(target);
            visitor.visit_expr(value);
        }
        TForInit::Expr(expr) => visitor.visit_expr(expr),
    }
}

pub fn walk_case<V: ThirVisitor + ?Sized>(visitor: &mut V, case: &TCase) {
    visitor.visit_pattern(&case.pattern);
    for stmt in &case.statements {
        visitor.visit_stmt(stmt);
    }
}

pub fn walk_pattern<V: ThirVisitor + ?Sized>(visitor: &mut V, pattern: &TPattern) {
    match pattern {
        TPattern::Wildcard { .. } | TPattern::Binding { .. } => {}
        TPattern::Variant { payload, .. } => {
            for pattern in payload {
                visitor.visit_pattern(pattern);
            }
        }
    }
}

pub fn walk_expr<V: ThirVisitor + ?Sized>(visitor: &mut V, expr: &TExpr) {
    match &expr.kind {
        TExprKind::Local(..)
        | TExprKind::Function(..)
        | TExprKind::GenericFunction { .. }
        | TExprKind::Literal(_)
        | TExprKind::TypeSize { .. }
        | TExprKind::TypeAlign { .. } => {}
        TExprKind::StructLiteral { fields, .. } => {
            for (_, value) in fields {
                visitor.visit_expr(value);
            }
        }
        TExprKind::EnumLiteral { payload, .. } => {
            for value in payload {
                visitor.visit_expr(value);
            }
        }
        TExprKind::ArrayLiteral(elements) => {
            for element in elements {
                visitor.visit_expr(element);
            }
        }
        TExprKind::ArrayRepeat { element, .. } => visitor.visit_expr(element),
        TExprKind::Closure { body, .. } => visitor.visit_closure_body(body),
        TExprKind::Move(inner)
        | TExprKind::FunctionToClosure(inner)
        | TExprKind::RetainClosure { expr: inner, .. }
        | TExprKind::Unary { expr: inner, .. }
        | TExprKind::Cast { expr: inner, .. }
        | TExprKind::Try { expr: inner, .. }
        | TExprKind::AsyncBlockOn { future: inner }
        | TExprKind::ArrayToSlice(inner)
        | TExprKind::SliceToConst(inner)
        | TExprKind::MakeDynamicInterface { expr: inner, .. }
        | TExprKind::ErrorBox { expr: inner, .. } => visitor.visit_expr(inner),
        TExprKind::RawSliceFromPtr { ptr, len, .. } => {
            visitor.visit_expr(ptr);
            visitor.visit_expr(len);
        }
        TExprKind::Await { future: inner } => visitor.visit_expr(inner),
        TExprKind::AsyncSelect { arms, .. } => {
            for arm in arms {
                visitor.visit_expr(&arm.future);
                visitor.visit_expr(&arm.body);
            }
        }
        TExprKind::AsyncSleep { ms, output_ty: _ } => visitor.visit_expr(ms),
        TExprKind::AsyncOpFuture { op, .. } => visitor.visit_expr(op),
        TExprKind::AsyncSpawn { body, .. } => visitor.visit_expr(body),
        TExprKind::AsyncTaskCancel { task, .. } | TExprKind::AsyncTaskIsFinished { task, .. } => {
            visitor.visit_expr(task)
        }
        TExprKind::AsyncChannelSend { sender, value, .. }
        | TExprKind::AsyncChannelTrySend { sender, value, .. } => {
            visitor.visit_expr(sender);
            visitor.visit_expr(value);
        }
        TExprKind::AsyncChannelReserve { sender, .. } => visitor.visit_expr(sender),
        TExprKind::AsyncChannelRecv { receiver, .. } => visitor.visit_expr(receiver),
        TExprKind::AsyncChannelPermitSend { permit, value, .. } => {
            visitor.visit_expr(permit);
            visitor.visit_expr(value);
        }
        TExprKind::UnsafeBlock { statements, value } => {
            for stmt in statements {
                visitor.visit_stmt(stmt);
            }
            if let Some(value) = value {
                visitor.visit_expr(value);
            }
        }
        TExprKind::Binary { left, right, .. } => {
            visitor.visit_expr(left);
            visitor.visit_expr(right);
        }
        TExprKind::Call { callee, args } => {
            visitor.visit_expr(callee);
            for arg in args {
                visitor.visit_expr(arg);
            }
        }
        TExprKind::DynamicInterfaceCall { receiver, args, .. }
        | TExprKind::RetainedClosureInterfaceCall { receiver, args, .. } => {
            visitor.visit_expr(receiver);
            for arg in args {
                visitor.visit_expr(arg);
            }
        }
        TExprKind::CloneMessage { value, .. } => visitor.visit_expr(value),
        TExprKind::Field { base, .. } | TExprKind::Arrow { base, .. } => {
            visitor.visit_expr(base);
        }
        TExprKind::Index { base, index } => {
            visitor.visit_expr(base);
            visitor.visit_expr(index);
        }
        TExprKind::Slice { base, start, end } => {
            visitor.visit_expr(base);
            if let Some(start) = start {
                visitor.visit_expr(start);
            }
            if let Some(end) = end {
                visitor.visit_expr(end);
            }
        }
        TExprKind::MetaAsRefRepr { value, .. }
        | TExprKind::MetaIntoRepr { value, .. }
        | TExprKind::MetaFromRepr { value, .. } => visitor.visit_expr(value),
        TExprKind::ActorSpawn {
            state_arg, handler, ..
        } => {
            visitor.visit_expr(state_arg);
            visitor.visit_expr(handler);
        }
        TExprKind::ActorSend { actor, value, .. } => {
            visitor.visit_expr(actor);
            visitor.visit_expr(value);
        }
        TExprKind::ActorStop { actor, .. } | TExprKind::ActorJoin { actor, .. } => {
            visitor.visit_expr(actor);
        }
    }
}

pub fn walk_closure_body<V: ThirVisitor + ?Sized>(visitor: &mut V, body: &TClosureBody) {
    match body {
        TClosureBody::Expr(expr) => visitor.visit_expr(expr),
        TClosureBody::Block(block) => visitor.visit_block(block),
    }
}

struct ExprChildVisitor<'a, F: FnMut(&TExpr)> {
    visit: &'a mut F,
}

impl<F: FnMut(&TExpr)> ThirVisitor for ExprChildVisitor<'_, F> {
    fn visit_expr(&mut self, expr: &TExpr) {
        (self.visit)(expr);
    }
}

pub fn walk_expr_children<F: FnMut(&TExpr)>(expr: &TExpr, visit: &mut F) {
    let mut visitor = ExprChildVisitor { visit };
    walk_expr(&mut visitor, expr);
}
