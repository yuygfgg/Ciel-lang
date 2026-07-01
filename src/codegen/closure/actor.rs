use super::*;

impl<'a> CGenerator<'a> {
    pub(in crate::codegen) fn emit_actor_dispatch_prototypes(&mut self) {
        let dispatches = self.plan.actor_dispatches.clone();
        for dispatch in dispatches.values() {
            self.line(&format!(
                "static void {}(CielActor *actor_raw, void *state_raw, void *handler_raw, void *message_raw, int32_t *failed);",
                dispatch.name
            ));
        }
    }

    pub(in crate::codegen) fn emit_actor_dispatches(&mut self) -> DiagResult<()> {
        let dispatches = self.plan.actor_dispatches.clone();
        for dispatch in dispatches.values() {
            self.emit_actor_dispatch(dispatch)?;
            self.line("");
        }
        Ok(())
    }

    fn emit_actor_dispatch(&mut self, dispatch: &ActorDispatch) -> DiagResult<()> {
        let result_ty = match dispatch.mode {
            ActorSpawnMode::Cloned => self.callable_ret_params(&dispatch.handler_ty)?.0,
            ActorSpawnMode::State => std_result_ty(Ty::Void, std_error_ty()),
        };
        let result_layout = self.result_layout(
            &result_ty,
            crate::span::Span::new(crate::span::FileId(0), 0, 0),
        )?;
        self.line(&format!(
            "static void {}(CielActor *actor_raw, void *state_raw, void *handler_raw, void *message_raw, int32_t *failed) {{",
            dispatch.name
        ));
        self.line_indent(
            1,
            &format!(
                "{} = state_raw;",
                self.c_pointer_decl(&dispatch.state_ty, "state")
            ),
        );
        self.line_indent(
            1,
            &format!(
                "{} = handler_raw;",
                self.c_pointer_decl(&dispatch.handler_ty, "handler")
            ),
        );
        self.line_indent(
            1,
            &format!(
                "{} = message_raw;",
                self.c_pointer_decl(&dispatch.message_ty, "message")
            ),
        );
        let call = match dispatch.mode {
            ActorSpawnMode::Cloned => self.callable_call_expr(
                &dispatch.handler_ty,
                "(*handler)",
                &["(*state)", "(*message)"],
            )?,
            ActorSpawnMode::State => {
                let actor_ty = Ty::Named {
                    name: "Actor".to_string(),
                    args: vec![dispatch.handle_message_ty.clone()],
                };
                self.line_indent(
                    1,
                    &format!(
                        "{} = ({}){{ .handle = (void *)actor_raw }};",
                        self.c_decl(&actor_ty, "actor_self"),
                        self.c_type(&actor_ty)
                    ),
                );
                self.callable_call_expr(
                    &dispatch.handler_ty,
                    "(*handler)",
                    &["state", "actor_self", "(*message)"],
                )?
            }
        };
        self.line_indent(
            1,
            &format!("{} result = {call};", self.c_decl(&result_ty, "")),
        );
        self.line_indent(
            1,
            &format!("if (result.tag == {}) {{", result_layout.err_index),
        );
        self.line_indent(2, "*failed = 1;");
        self.line_indent(2, "return;");
        self.line_indent(1, "}");
        if matches!(dispatch.mode, ActorSpawnMode::Cloned) && result_layout.ok_has_payload {
            self.line_indent(
                1,
                &format!("*state = result.as.{}._0;", result_layout.ok_name),
            );
        }
        self.line("}");
        Ok(())
    }
}
