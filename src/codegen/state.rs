use super::*;

pub(super) struct CGenerator<'a> {
    pub(super) program: &'a MonoProgram,
    pub(super) escapes: &'a EscapeProgram,
    pub(super) source_map: &'a SourceMap,
    pub(super) out: String,
    pub(super) plan: CodegenPlanData,
    pub(super) current_heap_locals: HashSet<LocalId>,
    pub(super) current_param_locals: HashMap<LocalId, String>,
    pub(super) current_capture_locals: HashMap<LocalId, String>,
    pub(super) defer_stack: Vec<Vec<String>>,
    pub(super) temporary_resource_cleanup_depth: usize,
    pub(super) temporary_resource_cleanup_frames: Vec<usize>,
    pub(super) current_owned_resource_roots: Vec<(Ty, String)>,
    pub(super) loop_defer_starts: Vec<usize>,
    pub(super) break_defer_starts: Vec<usize>,
    pub(super) continue_targets: Vec<Option<String>>,
    pub(super) current_return_ty: Ty,
    pub(super) current_async_output: Option<String>,
    pub(super) current_async_context: Option<String>,
    pub(super) current_async_await_index: usize,
    pub(super) current_async_frame_locals: HashMap<LocalId, String>,
    pub(super) current_async_await_outputs: Vec<Option<(String, Ty)>>,
    pub(super) current_async_defer_arg_index: usize,
    pub(super) current_async_cleanup_cases: Vec<Vec<Vec<String>>>,
    pub(super) temp_counter: usize,
    pub(super) share_handle_templates: Vec<Ty>,
    pub(super) thread_local_templates: Vec<Ty>,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum ResourceScopedCall {
    Default,
    WithLimits,
}

#[derive(Clone, Debug, Default)]
pub(super) struct CodegenPlanData {
    pub(super) array_return_types: BTreeMap<String, Ty>,
    pub(super) slice_types: BTreeMap<String, Ty>,
    pub(super) dynamic_types: BTreeMap<String, Ty>,
    pub(super) dynamic_impls: BTreeMap<String, DynamicImplUse>,
    pub(super) closure_types: BTreeMap<String, Ty>,
    pub(super) closure_defs: BTreeMap<ClosureInstanceId, ClosureDef>,
    pub(super) function_closure_wrappers: BTreeMap<String, FunctionClosureWrapper>,
    pub(super) retained_closure_wrappers: BTreeMap<String, RetainedClosureWrapper>,
    pub(super) retained_closure_witnesses: BTreeMap<String, RetainedClosureWitness>,
    pub(super) actor_dispatches: BTreeMap<String, ActorDispatch>,
    pub(super) async_sleep_output_tys: BTreeMap<String, Ty>,
    pub(super) resource_cleanup_tys: BTreeMap<String, Ty>,
    pub(super) string_literals: BTreeMap<(usize, usize, usize), String>,
    pub(super) string_literal_names: HashMap<(usize, usize, usize), String>,
    pub(super) source_locations: BTreeMap<(usize, usize), SourceLocation>,
    pub(super) type_ids: Vec<Ty>,
    pub(super) name_map: HashMap<DefId, String>,
}

#[derive(Clone, Debug)]
pub(super) struct DynamicImplUse {
    pub(super) dyn_ty: Ty,
    pub(super) concrete_ty: Ty,
}

#[derive(Clone, Debug)]
pub(super) struct ClosureDef {
    pub(super) id: ClosureInstanceId,
    pub(super) function_def: DefId,
    pub(super) ty: Ty,
    pub(super) is_async: bool,
    pub(super) async_facts: Option<AsyncFacts>,
    pub(super) params: Vec<(LocalId, String, Ty)>,
    pub(super) captures: Vec<TClosureCapture>,
    pub(super) body: TClosureBody,
}

#[derive(Clone, Debug)]
pub(super) struct FunctionClosureWrapper {
    pub(super) closure_ty: Ty,
    pub(super) function_ty: Ty,
}

#[derive(Clone, Debug)]
pub(super) struct RetainedClosureWrapper {
    pub(super) target_ty: Ty,
    pub(super) source_ty: Ty,
}

#[derive(Clone, Debug)]
pub(super) struct RetainedClosureWitness {
    pub(super) target_ty: Ty,
    pub(super) source_ty: Ty,
    pub(super) capability: ConstraintRef,
    pub(super) span: crate::span::Span,
}

#[derive(Clone, Debug)]
pub(super) struct ActorDispatch {
    pub(super) name: String,
    pub(super) mode: ActorSpawnMode,
    pub(super) state_ty: Ty,
    pub(super) handle_message_ty: Ty,
    pub(super) message_ty: Ty,
    pub(super) handler_ty: Ty,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AsyncClosureCaptureInit {
    Copy,
    CloneForTask,
}

#[derive(Clone, Debug)]
pub(super) struct AsyncFunctionNames {
    pub(super) context: String,
    pub(super) run: String,
    pub(super) cleanup: String,
}

#[derive(Clone, Debug)]
pub(super) struct SourceLocation {
    pub(super) name: String,
    pub(super) file: String,
    pub(super) line: usize,
}

#[derive(Clone, Debug)]
pub(super) struct MetaProductField {
    pub(super) name: String,
    pub(super) ty: Ty,
    pub(super) value_expr: String,
}

#[derive(Clone, Debug)]
pub(super) struct MetaCaptureField {
    pub(super) index: usize,
    pub(super) ty: Ty,
}

#[derive(Clone, Debug)]
pub(super) struct MetaPayloadField {
    pub(super) index: usize,
    pub(super) ty: Ty,
    pub(super) value_expr: String,
}

#[derive(Clone, Debug)]
pub(super) struct MetaSchemaField {
    pub(super) name: String,
    pub(super) source_ty: Ty,
    pub(super) repr_ty: Ty,
}

#[derive(Clone, Debug)]
pub(super) struct MetaSchemaPayload {
    pub(super) index: usize,
    pub(super) source_ty: Ty,
    pub(super) repr_ty: Ty,
}

#[derive(Clone, Debug)]
pub(super) struct ResultLayout {
    pub(super) c_type: String,
    pub(super) ok_index: usize,
    pub(super) ok_name: String,
    pub(super) ok_has_payload: bool,
    pub(super) ok_payload_ty: Option<Ty>,
    pub(super) err_name: String,
    pub(super) err_index: usize,
    pub(super) err_has_payload: bool,
    pub(super) err_payload_ty: Option<Ty>,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum AggregateLayoutRef {
    Struct(usize),
    Enum(usize),
}

impl<'a> CGenerator<'a> {
    pub(super) fn new(
        program: &'a MonoProgram,
        escapes: &'a EscapeProgram,
        source_map: &'a SourceMap,
    ) -> Self {
        Self {
            program,
            escapes,
            source_map,
            out: String::new(),
            plan: CodegenPlanData::default(),
            current_heap_locals: HashSet::new(),
            current_param_locals: HashMap::new(),
            current_capture_locals: HashMap::new(),
            defer_stack: Vec::new(),
            temporary_resource_cleanup_depth: 0,
            temporary_resource_cleanup_frames: Vec::new(),
            current_owned_resource_roots: Vec::new(),
            loop_defer_starts: Vec::new(),
            break_defer_starts: Vec::new(),
            continue_targets: Vec::new(),
            current_return_ty: Ty::Void,
            current_async_output: None,
            current_async_context: None,
            current_async_await_index: 0,
            current_async_frame_locals: HashMap::new(),
            current_async_await_outputs: Vec::new(),
            current_async_defer_arg_index: 0,
            current_async_cleanup_cases: Vec::new(),
            temp_counter: 0,
            share_handle_templates: program.checked.share_handle_templates.clone(),
            thread_local_templates: program.checked.thread_local_templates.clone(),
        }
    }
}
