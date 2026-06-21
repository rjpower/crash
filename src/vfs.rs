//! In-memory virtual filesystem.
//!
//! The VFS is the single source of truth for all simulated file state. It is a flat
//! `BTreeMap<path, Node>` keyed by clean absolute paths ("/" is the root). This keeps
//! directory listing, rename, copy and snapshotting trivial at the scale of an RL task
//! (thousands of files), while still supporting unix permissions, ownership and symlinks.

use std::collections::BTreeMap;

pub type Mode = u32;

#[derive(Clone, Debug)]
pub enum NodeKind {
    File(Vec<u8>),
    Dir,
    Symlink(String),
}

#[derive(Clone, Debug)]
pub struct Node {
    pub kind: NodeKind,
    pub mode: Mode,
    pub uid: u32,
    pub gid: u32,
    /// virtual modification time in milliseconds (from the simulated clock)
    pub mtime: u64,
}

impl Node {
    fn dir(mode: Mode) -> Self {
        Node { kind: NodeKind::Dir, mode, uid: 0, gid: 0, mtime: 0 }
    }
    fn file(data: Vec<u8>, mode: Mode) -> Self {
        Node { kind: NodeKind::File(data), mode, uid: 0, gid: 0, mtime: 0 }
    }
}

#[derive(Debug)]
pub enum VfsError {
    NotFound(String),
    NotADir(String),
    IsADir(String),
    NotEmpty(String),
    Exists(String),
    Loop(String),
    Invalid(String),
}

impl std::fmt::Display for VfsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VfsError::NotFound(p) => write!(f, "No such file or directory: {p}"),
            VfsError::NotADir(p) => write!(f, "Not a directory: {p}"),
            VfsError::IsADir(p) => write!(f, "Is a directory: {p}"),
            VfsError::NotEmpty(p) => write!(f, "Directory not empty: {p}"),
            VfsError::Exists(p) => write!(f, "File exists: {p}"),
            VfsError::Loop(p) => write!(f, "Too many levels of symbolic links: {p}"),
            VfsError::Invalid(p) => write!(f, "Invalid argument: {p}"),
        }
    }
}

pub type Result<T> = std::result::Result<T, VfsError>;

#[derive(Clone)]
pub struct Vfs {
    nodes: BTreeMap<String, Node>,
}

impl Default for Vfs {
    fn default() -> Self {
        Self::new()
    }
}

/// Normalize an absolute path lexically (resolve "." and "..", collapse "//"),
/// WITHOUT resolving symlinks. Input must be absolute. Result has no trailing
/// slash except for the root "/".
pub fn normalize(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for comp in path.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    if out.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", out.join("/"))
    }
}

/// Join a (possibly relative) path onto cwd and normalize lexically.
pub fn resolve_against(cwd: &str, path: &str) -> String {
    if path.starts_with('/') {
        normalize(path)
    } else {
        normalize(&format!("{cwd}/{path}"))
    }
}

pub fn parent_of(path: &str) -> Option<String> {
    if path == "/" {
        return None;
    }
    match path.rfind('/') {
        Some(0) => Some("/".to_string()),
        Some(i) => Some(path[..i].to_string()),
        None => None,
    }
}

pub fn basename(path: &str) -> &str {
    if path == "/" {
        return "/";
    }
    match path.rfind('/') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

impl Vfs {
    pub fn new() -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert("/".to_string(), Node::dir(0o755));
        Vfs { nodes }
    }

    // ---- low level ----

    pub fn raw_get(&self, abs: &str) -> Option<&Node> {
        self.nodes.get(abs)
    }

    /// Resolve symlinks along a normalized absolute path, returning the final real path.
    /// `follow_final` controls whether a trailing symlink is itself dereferenced.
    pub fn realpath(&self, abs: &str, follow_final: bool) -> Result<String> {
        self.realpath_inner(abs, follow_final, 0)
    }

    fn realpath_inner(&self, abs: &str, follow_final: bool, depth: usize) -> Result<String> {
        if depth > 40 {
            return Err(VfsError::Loop(abs.to_string()));
        }
        if abs == "/" {
            return Ok("/".to_string());
        }
        let parent = parent_of(abs).unwrap_or_else(|| "/".to_string());
        let real_parent = self.realpath_inner(&parent, true, depth + 1)?;
        let name = basename(abs);
        let candidate = if real_parent == "/" {
            format!("/{name}")
        } else {
            format!("{real_parent}/{name}")
        };
        match self.nodes.get(&candidate) {
            Some(Node { kind: NodeKind::Symlink(target), .. }) if follow_final => {
                let next = resolve_against(&real_parent, target);
                self.realpath_inner(&next, true, depth + 1)
            }
            _ => Ok(candidate),
        }
    }

    pub fn exists(&self, cwd: &str, path: &str) -> bool {
        let abs = resolve_against(cwd, path);
        self.realpath(&abs, true).map(|p| self.nodes.contains_key(&p)).unwrap_or(false)
    }

    pub fn lexists(&self, cwd: &str, path: &str) -> bool {
        let abs = resolve_against(cwd, path);
        self.realpath(&abs, false).map(|p| self.nodes.contains_key(&p)).unwrap_or(false)
    }

    pub fn is_dir(&self, cwd: &str, path: &str) -> bool {
        let abs = resolve_against(cwd, path);
        match self.realpath(&abs, true) {
            Ok(p) => matches!(self.nodes.get(&p), Some(Node { kind: NodeKind::Dir, .. })),
            Err(_) => false,
        }
    }

    pub fn is_file(&self, cwd: &str, path: &str) -> bool {
        let abs = resolve_against(cwd, path);
        match self.realpath(&abs, true) {
            Ok(p) => matches!(self.nodes.get(&p), Some(Node { kind: NodeKind::File(_), .. })),
            Err(_) => false,
        }
    }

    pub fn is_symlink(&self, cwd: &str, path: &str) -> bool {
        let abs = resolve_against(cwd, path);
        match self.realpath(&abs, false) {
            Ok(p) => matches!(self.nodes.get(&p), Some(Node { kind: NodeKind::Symlink(_), .. })),
            Err(_) => false,
        }
    }

    // ---- reads ----

    pub fn read(&self, cwd: &str, path: &str) -> Result<Vec<u8>> {
        let abs = resolve_against(cwd, path);
        let real = self.realpath(&abs, true)?;
        match self.nodes.get(&real) {
            Some(Node { kind: NodeKind::File(d), .. }) => Ok(d.clone()),
            Some(Node { kind: NodeKind::Dir, .. }) => Err(VfsError::IsADir(path.to_string())),
            _ => Err(VfsError::NotFound(path.to_string())),
        }
    }

    pub fn read_string(&self, cwd: &str, path: &str) -> Result<String> {
        Ok(String::from_utf8_lossy(&self.read(cwd, path)?).into_owned())
    }

    pub fn metadata(&self, cwd: &str, path: &str, follow: bool) -> Result<Node> {
        let abs = resolve_against(cwd, path);
        let real = self.realpath(&abs, follow)?;
        self.nodes.get(&real).cloned().ok_or_else(|| VfsError::NotFound(path.to_string()))
    }

    /// List directory entry names (not including "." / "..").
    pub fn list_dir(&self, cwd: &str, path: &str) -> Result<Vec<String>> {
        let abs = resolve_against(cwd, path);
        let real = self.realpath(&abs, true)?;
        match self.nodes.get(&real) {
            Some(Node { kind: NodeKind::Dir, .. }) => {}
            Some(_) => return Err(VfsError::NotADir(path.to_string())),
            None => return Err(VfsError::NotFound(path.to_string())),
        }
        let prefix = if real == "/" { "/".to_string() } else { format!("{real}/") };
        let mut out = Vec::new();
        for key in self.nodes.keys() {
            if let Some(rest) = key.strip_prefix(&prefix) {
                if !rest.is_empty() && !rest.contains('/') {
                    out.push(rest.to_string());
                }
            }
        }
        out.sort();
        Ok(out)
    }

    /// All paths under `dir` (recursive), including the dir itself, sorted.
    pub fn walk(&self, abs_dir: &str) -> Vec<String> {
        let prefix = if abs_dir == "/" { "/".to_string() } else { format!("{abs_dir}/") };
        let mut out = Vec::new();
        for key in self.nodes.keys() {
            if key == abs_dir || key.starts_with(&prefix) {
                out.push(key.clone());
            }
        }
        out
    }

    pub fn read_link(&self, cwd: &str, path: &str) -> Result<String> {
        let abs = resolve_against(cwd, path);
        let real = self.realpath(&abs, false)?;
        match self.nodes.get(&real) {
            Some(Node { kind: NodeKind::Symlink(t), .. }) => Ok(t.clone()),
            _ => Err(VfsError::Invalid(path.to_string())),
        }
    }

    // ---- writes ----

    fn require_parent_dir(&self, abs: &str) -> Result<()> {
        let parent = parent_of(abs).unwrap_or_else(|| "/".to_string());
        let real_parent = self.realpath(&parent, true)?;
        match self.nodes.get(&real_parent) {
            Some(Node { kind: NodeKind::Dir, .. }) => Ok(()),
            Some(_) => Err(VfsError::NotADir(parent)),
            None => Err(VfsError::NotFound(parent)),
        }
    }

    /// Map a path to where the node should actually live (parent symlinks resolved).
    fn write_target(&self, cwd: &str, path: &str) -> Result<String> {
        let abs = resolve_against(cwd, path);
        let parent = parent_of(&abs).unwrap_or_else(|| "/".to_string());
        let real_parent = self.realpath(&parent, true)?;
        let name = basename(&abs);
        Ok(if real_parent == "/" { format!("/{name}") } else { format!("{real_parent}/{name}") })
    }

    pub fn write(&mut self, cwd: &str, path: &str, data: &[u8], mode: Mode) -> Result<()> {
        let target = self.write_target(cwd, path)?;
        self.require_parent_dir(&target)?;
        match self.nodes.get_mut(&target) {
            Some(Node { kind: NodeKind::File(d), .. }) => {
                *d = data.to_vec();
            }
            Some(Node { kind: NodeKind::Dir, .. }) => {
                return Err(VfsError::IsADir(path.to_string()))
            }
            _ => {
                self.nodes.insert(target, Node::file(data.to_vec(), mode));
            }
        }
        Ok(())
    }

    pub fn append(&mut self, cwd: &str, path: &str, data: &[u8], mode: Mode) -> Result<()> {
        let target = self.write_target(cwd, path)?;
        self.require_parent_dir(&target)?;
        match self.nodes.get_mut(&target) {
            Some(Node { kind: NodeKind::File(d), .. }) => d.extend_from_slice(data),
            Some(Node { kind: NodeKind::Dir, .. }) => {
                return Err(VfsError::IsADir(path.to_string()))
            }
            _ => {
                self.nodes.insert(target, Node::file(data.to_vec(), mode));
            }
        }
        Ok(())
    }

    pub fn mkdir(&mut self, cwd: &str, path: &str) -> Result<()> {
        let target = self.write_target(cwd, path)?;
        if self.nodes.contains_key(&target) {
            return Err(VfsError::Exists(path.to_string()));
        }
        self.require_parent_dir(&target)?;
        self.nodes.insert(target, Node::dir(0o755));
        Ok(())
    }

    pub fn mkdir_all(&mut self, cwd: &str, path: &str) -> Result<()> {
        let abs = resolve_against(cwd, path);
        let comps: Vec<&str> = abs.split('/').filter(|c| !c.is_empty()).collect();
        // resolve symlinks progressively
        let mut cur = "/".to_string();
        for c in comps {
            let next = if cur == "/" { format!("/{c}") } else { format!("{cur}/{c}") };
            let real = self.realpath(&next, true).unwrap_or(next.clone());
            match self.nodes.get(&real) {
                Some(Node { kind: NodeKind::Dir, .. }) => {}
                Some(_) => return Err(VfsError::NotADir(real)),
                None => {
                    self.nodes.insert(real.clone(), Node::dir(0o755));
                }
            }
            cur = real;
        }
        Ok(())
    }

    pub fn remove_file(&mut self, cwd: &str, path: &str) -> Result<()> {
        let abs = resolve_against(cwd, path);
        let real = self.realpath(&abs, false)?;
        match self.nodes.get(&real) {
            Some(Node { kind: NodeKind::Dir, .. }) => Err(VfsError::IsADir(path.to_string())),
            Some(_) => {
                self.nodes.remove(&real);
                Ok(())
            }
            None => Err(VfsError::NotFound(path.to_string())),
        }
    }

    pub fn remove_all(&mut self, cwd: &str, path: &str) -> Result<()> {
        let abs = resolve_against(cwd, path);
        let real = self.realpath(&abs, false)?;
        if !self.nodes.contains_key(&real) {
            return Err(VfsError::NotFound(path.to_string()));
        }
        for k in self.walk(&real) {
            self.nodes.remove(&k);
        }
        Ok(())
    }

    pub fn rmdir(&mut self, cwd: &str, path: &str) -> Result<()> {
        let abs = resolve_against(cwd, path);
        let real = self.realpath(&abs, true)?;
        match self.nodes.get(&real) {
            Some(Node { kind: NodeKind::Dir, .. }) => {
                if !self.list_dir(cwd, path)?.is_empty() {
                    return Err(VfsError::NotEmpty(path.to_string()));
                }
                self.nodes.remove(&real);
                Ok(())
            }
            Some(_) => Err(VfsError::NotADir(path.to_string())),
            None => Err(VfsError::NotFound(path.to_string())),
        }
    }

    pub fn rename(&mut self, cwd: &str, from: &str, to: &str) -> Result<()> {
        let from_abs = resolve_against(cwd, from);
        let from_real = self.realpath(&from_abs, false)?;
        if !self.nodes.contains_key(&from_real) {
            return Err(VfsError::NotFound(from.to_string()));
        }
        let mut to_target = self.write_target(cwd, to)?;
        // moving into an existing directory
        if matches!(self.nodes.get(&to_target), Some(Node { kind: NodeKind::Dir, .. })) {
            let name = basename(&from_real);
            to_target = if to_target == "/" { format!("/{name}") } else { format!("{to_target}/{name}") };
        }
        let subtree = self.walk(&from_real);
        for k in subtree {
            let node = self.nodes.remove(&k).unwrap();
            let suffix = &k[from_real.len()..];
            let newk = format!("{to_target}{suffix}");
            self.nodes.insert(newk, node);
        }
        Ok(())
    }

    pub fn copy_file(&mut self, cwd: &str, from: &str, to: &str) -> Result<()> {
        let data = self.read(cwd, from)?;
        let mode = self.metadata(cwd, from, true)?.mode;
        self.write(cwd, to, &data, mode)
    }

    pub fn copy_recursive(&mut self, cwd: &str, from: &str, to: &str) -> Result<()> {
        let from_abs = resolve_against(cwd, from);
        let from_real = self.realpath(&from_abs, true)?;
        if !self.is_dir(cwd, from) {
            return self.copy_file(cwd, from, to);
        }
        let mut to_target = self.write_target(cwd, to)?;
        if matches!(self.nodes.get(&to_target), Some(Node { kind: NodeKind::Dir, .. })) {
            let name = basename(&from_real);
            to_target = if to_target == "/" { format!("/{name}") } else { format!("{to_target}/{name}") };
        }
        for k in self.walk(&from_real) {
            let node = self.nodes.get(&k).unwrap().clone();
            let suffix = &k[from_real.len()..];
            let newk = format!("{to_target}{suffix}");
            self.nodes.insert(newk, node);
        }
        Ok(())
    }

    pub fn symlink(&mut self, cwd: &str, target: &str, linkpath: &str) -> Result<()> {
        let link_target = self.write_target(cwd, linkpath)?;
        if self.nodes.contains_key(&link_target) {
            return Err(VfsError::Exists(linkpath.to_string()));
        }
        self.require_parent_dir(&link_target)?;
        self.nodes.insert(
            link_target,
            Node { kind: NodeKind::Symlink(target.to_string()), mode: 0o777, uid: 0, gid: 0, mtime: 0 },
        );
        Ok(())
    }

    pub fn touch(&mut self, cwd: &str, path: &str, mtime: u64) -> Result<()> {
        let target = self.write_target(cwd, path)?;
        match self.nodes.get_mut(&target) {
            Some(n) => n.mtime = mtime,
            None => {
                self.require_parent_dir(&target)?;
                let mut n = Node::file(Vec::new(), 0o644);
                n.mtime = mtime;
                self.nodes.insert(target, n);
            }
        }
        Ok(())
    }

    pub fn chmod(&mut self, cwd: &str, path: &str, mode: Mode) -> Result<()> {
        let abs = resolve_against(cwd, path);
        let real = self.realpath(&abs, true)?;
        self.nodes
            .get_mut(&real)
            .map(|n| n.mode = mode & 0o7777)
            .ok_or_else(|| VfsError::NotFound(path.to_string()))
    }

    pub fn chown(&mut self, cwd: &str, path: &str, uid: Option<u32>, gid: Option<u32>) -> Result<()> {
        let abs = resolve_against(cwd, path);
        let real = self.realpath(&abs, true)?;
        let n = self.nodes.get_mut(&real).ok_or_else(|| VfsError::NotFound(path.to_string()))?;
        if let Some(u) = uid {
            n.uid = u;
        }
        if let Some(g) = gid {
            n.gid = g;
        }
        Ok(())
    }

    pub fn set_mtime(&mut self, abs_real: &str, mtime: u64) {
        if let Some(n) = self.nodes.get_mut(abs_real) {
            n.mtime = mtime;
        }
    }

    // ---- bulk load / dump (bridge to real dirs for task loading & snapshots) ----

    /// Insert a file directly at an absolute path, creating parent dirs.
    pub fn put_file(&mut self, abs: &str, data: Vec<u8>, mode: Mode) {
        let norm = normalize(abs);
        if let Some(parent) = parent_of(&norm) {
            let _ = self.mkdir_all("/", &parent);
        }
        self.nodes.insert(norm, Node::file(data, mode));
    }

    pub fn put_dir(&mut self, abs: &str, mode: Mode) {
        let norm = normalize(abs);
        let _ = self.mkdir_all("/", &norm);
        if let Some(n) = self.nodes.get_mut(&norm) {
            n.mode = mode;
        }
    }

    pub fn all_paths(&self) -> impl Iterator<Item = (&String, &Node)> {
        self.nodes.iter()
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.len() <= 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_paths() {
        assert_eq!(normalize("/a/b/../c"), "/a/c");
        assert_eq!(normalize("/a/./b/"), "/a/b");
        assert_eq!(normalize("/"), "/");
        assert_eq!(normalize("/../.."), "/");
        assert_eq!(resolve_against("/home/x", "y/z"), "/home/x/y/z");
        assert_eq!(resolve_against("/home/x", "/etc"), "/etc");
    }

    #[test]
    fn basic_rw() {
        let mut v = Vfs::new();
        v.mkdir_all("/", "/a/b").unwrap();
        v.write("/a/b", "f.txt", b"hello", 0o644).unwrap();
        assert_eq!(v.read_string("/a/b", "f.txt").unwrap(), "hello");
        assert_eq!(v.read_string("/", "/a/b/f.txt").unwrap(), "hello");
        v.append("/", "/a/b/f.txt", b" world", 0o644).unwrap();
        assert_eq!(v.read_string("/", "/a/b/f.txt").unwrap(), "hello world");
        assert!(v.is_dir("/", "/a"));
        assert!(v.is_file("/", "/a/b/f.txt"));
        assert_eq!(v.list_dir("/", "/a/b").unwrap(), vec!["f.txt"]);
    }

    #[test]
    fn rename_and_copy() {
        let mut v = Vfs::new();
        v.mkdir_all("/", "/src").unwrap();
        v.write("/", "/src/a.txt", b"A", 0o644).unwrap();
        v.rename("/", "/src/a.txt", "/src/b.txt").unwrap();
        assert!(!v.is_file("/", "/src/a.txt"));
        assert_eq!(v.read_string("/", "/src/b.txt").unwrap(), "A");
        v.mkdir_all("/", "/dst").unwrap();
        v.copy_recursive("/", "/src", "/dst").unwrap();
        assert_eq!(v.read_string("/", "/dst/src/b.txt").unwrap(), "A");
    }

    #[test]
    fn symlinks() {
        let mut v = Vfs::new();
        v.mkdir_all("/", "/real").unwrap();
        v.write("/", "/real/f", b"data", 0o644).unwrap();
        v.symlink("/", "/real", "/link").unwrap();
        assert!(v.is_symlink("/", "/link"));
        assert_eq!(v.read_string("/", "/link/f").unwrap(), "data");
    }
}
