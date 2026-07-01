use super::*;

impl<'a> CGenerator<'a> {
    pub(in crate::codegen) fn async_facts_for_function<'b>(
        &self,
        function: &'b CheckedFunction,
    ) -> &'b AsyncFacts {
        function
            .async_facts
            .as_ref()
            .expect("async function facts are populated by typeck")
    }

    pub(in crate::codegen) fn async_facts_for_closure<'b>(
        &self,
        closure: &'b ClosureDef,
    ) -> &'b AsyncFacts {
        closure
            .async_facts
            .as_ref()
            .expect("async closure facts are populated by typeck")
    }

    pub(in crate::codegen) fn async_frame_locals_with_escape_info_for_function(
        &self,
        function: &CheckedFunction,
    ) -> Vec<AsyncFrameLocal> {
        let heap_locals = self
            .escapes
            .functions
            .get(&function.def_id)
            .map(|escape| escape.heap_locals.clone())
            .unwrap_or_default();
        self.async_facts_for_function(function)
            .frame_locals
            .iter()
            .cloned()
            .map(|mut local| {
                local.heap = heap_locals.contains(&local.id);
                local
            })
            .collect()
    }

    pub(in crate::codegen) fn async_frame_locals_with_escape_info_for_closure(
        &self,
        closure: &ClosureDef,
    ) -> Vec<AsyncFrameLocal> {
        let heap_locals = self
            .escapes
            .functions
            .get(&closure.owner)
            .map(|escape| escape.heap_locals.clone())
            .unwrap_or_default();
        self.async_facts_for_closure(closure)
            .frame_locals
            .iter()
            .cloned()
            .map(|mut local| {
                local.heap = heap_locals.contains(&local.id);
                local
            })
            .collect()
    }
}
