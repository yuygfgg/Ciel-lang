use super::*;

impl<'a> CGenerator<'a> {
    pub(in crate::codegen) fn emit_current_defers(&mut self, indent: usize) {
        if let Some(frame) = self.defer_stack.last() {
            let calls = frame.clone();
            for call in calls.iter().rev() {
                self.line_indent(indent, &format!("{call};"));
            }
        }
    }

    pub(in crate::codegen) fn emit_all_defers(&mut self, indent: usize) {
        let frames = self.defer_stack.clone();
        self.emit_defer_frames(&frames, indent);
    }

    pub(in crate::codegen) fn emit_defer_frames(&mut self, frames: &[Vec<String>], indent: usize) {
        for frame in frames.iter().rev() {
            for call in frame.iter().rev() {
                self.line_indent(indent, &format!("{call};"));
            }
        }
    }

    pub(in crate::codegen) fn emit_loop_defers(&mut self, indent: usize) {
        let start = self.loop_defer_starts.last().copied().unwrap_or(0);
        let frames = self.defer_stack.clone();
        for frame in frames.iter().skip(start).rev() {
            for call in frame.iter().rev() {
                self.line_indent(indent, &format!("{call};"));
            }
        }
    }

    pub(in crate::codegen) fn emit_break_defers(&mut self, indent: usize) {
        let start = self.break_defer_starts.last().copied().unwrap_or(0);
        let frames = self.defer_stack.clone();
        for frame in frames.iter().skip(start).rev() {
            for call in frame.iter().rev() {
                self.line_indent(indent, &format!("{call};"));
            }
        }
    }

    pub(in crate::codegen) fn next_temp(&mut self, prefix: &str) -> String {
        let id = self.temp_counter;
        self.temp_counter += 1;
        format!("ciel_{prefix}_{id}")
    }

    pub(in crate::codegen) fn local_is_heap(&self, id: LocalId) -> bool {
        self.current_heap_locals.contains(&id)
    }

    pub(in crate::codegen) fn local_is_async_frame(&self, id: LocalId) -> bool {
        self.current_async_frame_locals.contains_key(&id)
    }

    pub(in crate::codegen) fn local_c_name(&self, id: LocalId, source_name: &str) -> String {
        self.current_param_locals
            .get(&id)
            .cloned()
            .or_else(|| self.current_async_frame_locals.get(&id).cloned())
            .unwrap_or_else(|| format!("{source_name}__{}", id.0))
    }

    pub(in crate::codegen) fn local_value_expr(&self, id: LocalId, source_name: &str) -> String {
        let cname = self.local_c_name(id, source_name);
        if self.local_is_heap(id) {
            format!("(*{cname})")
        } else {
            cname
        }
    }

    pub(in crate::codegen) fn emit_line_directive(&mut self, span: crate::span::Span) {
        let file = self.source_map.file_path(span.file).display().to_string();
        let (line, _) = self.source_map.line_col(span.file, span.start);
        self.line(&format!("#line {line} \"{}\"", escape_c_string(&file)));
    }

    pub(in crate::codegen) fn location_args(&self, span: crate::span::Span) -> (String, String) {
        let file = self.source_map.file_path(span.file).display().to_string();
        let (line, _) = self.source_map.line_col(span.file, span.start);
        if let Some(location) = self.plan.source_locations.get(&(span.file.0, line)) {
            (
                format!("{}.file", location.name),
                format!("{}.line", location.name),
            )
        } else {
            (format!("\"{}\"", escape_c_string(&file)), line.to_string())
        }
    }

    pub(in crate::codegen) fn line(&mut self, text: &str) {
        self.out.push_str(text);
        self.out.push('\n');
    }

    pub(in crate::codegen) fn line_indent(&mut self, indent: usize, text: &str) {
        self.out.push_str(&"    ".repeat(indent));
        self.line(text);
    }
}
