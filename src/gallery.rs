use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::decode::extension;

const EXTENSIONS: &[&str] = &[
    "png", "apng", "jpg", "jpeg", "jpe", "jfif", "gif", "webp", "bmp", "ico", "tif", "tiff", "tga",
    "dds", "hdr", "exr", "pnm", "pbm", "pgm", "ppm", "pam", "ff", "qoi", "svg", "svgz", "heic",
    "heif", "hif", "avif", "jxl",
];

pub fn is_supported(path: &Path) -> bool {
    EXTENSIONS.contains(&extension(path).as_str())
}

pub struct Gallery {
    files: Vec<PathBuf>,
    index: usize,
}

impl Gallery {
    pub fn open(path: &Path) -> io::Result<Self> {
        let path = fs::canonicalize(path)?;
        let (dir, current) = if path.is_dir() {
            (path.clone(), None)
        } else {
            let dir = path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("/"));
            (dir, Some(path))
        };
        let mut files: Vec<PathBuf> = fs::read_dir(&dir)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|p| p.is_file() && is_supported(p))
            .collect();
        if let Some(current) = &current
            && !files.contains(current)
        {
            files.push(current.clone());
        }
        files.sort_by_cached_key(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default()
        });
        let index = current
            .and_then(|c| files.iter().position(|p| *p == c))
            .unwrap_or(0);
        Ok(Self { files, index })
    }

    pub fn current(&self) -> Option<&Path> {
        self.files.get(self.index).map(PathBuf::as_path)
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn position(&self) -> usize {
        self.index + 1
    }

    pub fn next(&mut self) {
        if !self.files.is_empty() {
            self.index = (self.index + 1) % self.files.len();
        }
    }

    pub fn prev(&mut self) {
        if !self.files.is_empty() {
            self.index = (self.index + self.files.len() - 1) % self.files.len();
        }
    }
}
