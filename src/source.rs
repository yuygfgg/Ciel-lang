use std::path::{Path, PathBuf};

use crate::span::FileId;

#[derive(Clone, Debug)]
pub struct SourceFile {
    pub id: FileId,
    pub path: PathBuf,
    pub text: String,
}

#[derive(Default, Debug)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

impl SourceMap {
    pub fn add(&mut self, path: PathBuf, text: String) -> FileId {
        let id = FileId(self.files.len());
        self.files.push(SourceFile { id, path, text });
        id
    }

    pub fn get(&self, id: FileId) -> &SourceFile {
        &self.files[id.0]
    }

    pub fn file_path(&self, id: FileId) -> &Path {
        &self.get(id).path
    }

    pub fn line_col(&self, id: FileId, byte: usize) -> (usize, usize) {
        let text = &self.get(id).text;
        let mut line = 1;
        let mut col = 1;
        for (idx, ch) in text.char_indices() {
            if idx >= byte {
                break;
            }
            if ch == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        (line, col)
    }
}
