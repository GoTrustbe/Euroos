//! In-memory ramdisk (EuroFS Fase 1).
//!
//! Bootstrap-filesysteem dat de kernel gebruikt vóór het echte on-disk EuroFS
//! klaar is: kernelmodules, init en config. Geen persistentie. Bewust simpel
//! en correct — geen performance-trucs. Wordt later vervangen door EuroFS.
//!
//! Implementatiekeuze: `BTreeMap<volledig_pad, Node>`. We gebruiken `BTreeMap`
//! en NIET `HashMap`: `HashMap` is niet beschikbaar in `no_std` + `alloc` (het
//! vereist een RNG voor DoS-bescherming).

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::fs::{DirEntry, EntryKind, FileSystem, FsError, FsResult};
use crate::path::{filename, join, normalize, parent};

#[derive(Clone)]
enum Node {
    File(Vec<u8>),
    /// Set van kindnamen (de inhoud zelf staat onder het volledige pad).
    Directory(BTreeMap<String, ()>),
}

pub struct RamDisk {
    nodes: BTreeMap<String, Node>,
    /// 0 = onbegrensd.
    max_size: u64,
    used_size: u64,
}

impl RamDisk {
    pub fn new(max_size: u64) -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert("/".to_string(), Node::Directory(BTreeMap::new()));
        Self {
            nodes,
            max_size,
            used_size: 0,
        }
    }

    /// Vul met initiële bestanden; maakt parent-directories automatisch aan.
    pub fn populate(&mut self, files: &[(&str, &[u8])]) {
        for (path, data) in files {
            let norm = normalize(path);
            self.ensure_dir_exists(parent(&norm));
            self.write_file(&norm, data).ok();
        }
    }

    fn ensure_dir_exists(&mut self, path: &str) {
        if path == "/" || self.exists(path) {
            return;
        }
        let parent_path = parent(path).to_string();
        self.ensure_dir_exists(&parent_path);
        self.create_dir(path).ok();
    }

    fn add_child(&mut self, parent_path: &str, name: &str) {
        if let Some(Node::Directory(children)) = self.nodes.get_mut(parent_path) {
            children.insert(name.to_string(), ());
        }
    }

    fn remove_child(&mut self, parent_path: &str, name: &str) {
        if let Some(Node::Directory(children)) = self.nodes.get_mut(parent_path) {
            children.remove(name);
        }
    }
}

impl FileSystem for RamDisk {
    fn read_file(&self, path: &str) -> FsResult<Vec<u8>> {
        match self.nodes.get(&normalize(path)) {
            Some(Node::File(data)) => Ok(data.clone()),
            Some(Node::Directory(_)) => Err(FsError::NotAFile),
            None => Err(FsError::NotFound),
        }
    }

    fn write_file(&mut self, path: &str, data: &[u8]) -> FsResult<()> {
        let norm = normalize(path);
        if norm == "/" {
            return Err(FsError::InvalidPath);
        }

        // Ruimtecontrole (delta t.o.v. bestaande inhoud).
        if self.max_size > 0 {
            let old = match self.nodes.get(&norm) {
                Some(Node::File(d)) => d.len() as u64,
                Some(Node::Directory(_)) => return Err(FsError::NotAFile),
                None => 0,
            };
            let new = data.len() as u64;
            let projected = self.used_size + new - old.min(self.used_size);
            if new > old && projected > self.max_size {
                return Err(FsError::NoSpace);
            }
            self.used_size = self.used_size - old + new;
        }

        let parent_path = parent(&norm).to_string();
        match self.nodes.get(&parent_path) {
            Some(Node::Directory(_)) => {}
            Some(_) => return Err(FsError::NotADirectory),
            None => return Err(FsError::NotFound),
        }

        let is_new = !self.nodes.contains_key(&norm);
        self.nodes.insert(norm.clone(), Node::File(data.to_vec()));
        if is_new {
            let name = filename(&norm).to_string();
            self.add_child(&parent_path, &name);
        }
        Ok(())
    }

    fn remove_file(&mut self, path: &str) -> FsResult<()> {
        let norm = normalize(path);
        match self.nodes.get(&norm) {
            Some(Node::File(data)) => {
                self.used_size = self.used_size.saturating_sub(data.len() as u64);
            }
            Some(Node::Directory(_)) => return Err(FsError::NotAFile),
            None => return Err(FsError::NotFound),
        }
        self.nodes.remove(&norm);
        self.remove_child(parent(&norm), filename(&norm));
        Ok(())
    }

    fn create_dir(&mut self, path: &str) -> FsResult<()> {
        let norm = normalize(path);
        if norm == "/" {
            return Ok(());
        }
        if self.nodes.contains_key(&norm) {
            return Err(FsError::AlreadyExists);
        }
        let parent_path = parent(&norm).to_string();
        match self.nodes.get(&parent_path) {
            Some(Node::Directory(_)) => {}
            Some(_) => return Err(FsError::NotADirectory),
            None => return Err(FsError::NotFound),
        }
        self.nodes
            .insert(norm.clone(), Node::Directory(BTreeMap::new()));
        self.add_child(&parent_path, filename(&norm));
        Ok(())
    }

    fn list_dir(&self, path: &str) -> FsResult<Vec<DirEntry>> {
        let norm = normalize(path);
        match self.nodes.get(&norm) {
            Some(Node::Directory(children)) => {
                let mut entries = Vec::with_capacity(children.len());
                for name in children.keys() {
                    let child = join(&norm, name);
                    let (kind, size) = match self.nodes.get(&child) {
                        Some(Node::File(d)) => (EntryKind::File, d.len() as u64),
                        Some(Node::Directory(_)) => (EntryKind::Directory, 0),
                        None => continue,
                    };
                    entries.push(DirEntry {
                        name: name.clone(),
                        kind,
                        size,
                        mode: if kind == EntryKind::Directory { 0o755 } else { 0o644 },
                        mtime: 0, // RAM-FS is vluchtig; geen wijzigingstijd
                    });
                }
                Ok(entries)
            }
            Some(_) => Err(FsError::NotADirectory),
            None => Err(FsError::NotFound),
        }
    }

    fn exists(&self, path: &str) -> bool {
        self.nodes.contains_key(&normalize(path))
    }

    fn metadata(&self, path: &str) -> FsResult<DirEntry> {
        let norm = normalize(path);
        let (kind, size) = match self.nodes.get(&norm) {
            Some(Node::File(d)) => (EntryKind::File, d.len() as u64),
            Some(Node::Directory(_)) => (EntryKind::Directory, 0),
            None => return Err(FsError::NotFound),
        };
        Ok(DirEntry {
            name: filename(&norm).to_string(),
            kind,
            size,
            mode: if kind == EntryKind::Directory { 0o755 } else { 0o644 },
            mtime: 0, // RAM-FS is vluchtig; geen wijzigingstijd
        })
    }

    fn space_info(&self) -> (u64, u64) {
        if self.max_size == 0 {
            (u64::MAX, u64::MAX)
        } else {
            (self.max_size, self.max_size.saturating_sub(self.used_size))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> RamDisk {
        RamDisk::new(1024 * 1024)
    }

    #[test]
    fn schrijf_en_lees() {
        let mut fs = fresh();
        // write_file maakt GEEN parents aan (zoals POSIX open): /etc moet bestaan.
        fs.create_dir("/etc").unwrap();
        fs.write_file("/etc/hostname", b"eurokernel\n").unwrap();
        assert_eq!(fs.read_file("/etc/hostname").unwrap(), b"eurokernel\n");
    }

    #[test]
    fn write_zonder_parent_faalt() {
        let mut fs = fresh();
        assert_eq!(fs.write_file("/geen/parent", b"x"), Err(FsError::NotFound));
    }

    #[test]
    fn populate_maakt_parents() {
        let mut fs = fresh();
        fs.populate(&[("/boot/grub/version", b"v0.1")]);
        assert!(fs.exists("/boot"));
        assert!(fs.exists("/boot/grub"));
        assert_eq!(fs.metadata("/boot").unwrap().kind, EntryKind::Directory);
        assert_eq!(fs.read_file("/boot/grub/version").unwrap(), b"v0.1");
    }

    #[test]
    fn lijst_directory() {
        let mut fs = fresh();
        fs.create_dir("/etc").unwrap();
        fs.write_file("/etc/a", b"1").unwrap();
        fs.write_file("/etc/b", b"22").unwrap();
        let mut entries = fs.list_dir("/etc").unwrap();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "a");
        assert_eq!(entries[1].size, 2);
    }

    #[test]
    fn overschrijven_update_ruimte() {
        let mut fs = fresh();
        fs.write_file("/f", b"aaaa").unwrap();
        let (_, free_after_4) = fs.space_info();
        fs.write_file("/f", b"a").unwrap();
        let (_, free_after_1) = fs.space_info();
        assert!(free_after_1 > free_after_4, "kleiner bestand = meer vrij");
        assert_eq!(fs.read_file("/f").unwrap(), b"a");
    }

    #[test]
    fn verwijderen_geeft_ruimte_terug() {
        let mut fs = RamDisk::new(100);
        fs.write_file("/big", &[0u8; 80]).unwrap();
        fs.remove_file("/big").unwrap();
        let (_, free) = fs.space_info();
        assert_eq!(free, 100);
        assert!(!fs.exists("/big"));
    }

    #[test]
    fn nospace_bij_overschrijding() {
        let mut fs = RamDisk::new(16);
        assert_eq!(fs.write_file("/x", &[0u8; 32]), Err(FsError::NoSpace));
    }

    #[test]
    fn fouten_op_rare_paden() {
        let mut fs = fresh();
        assert_eq!(fs.read_file("/bestaat-niet"), Err(FsError::NotFound));
        assert_eq!(fs.write_file("/", b"x"), Err(FsError::InvalidPath));
        fs.write_file("/file", b"x").unwrap();
        // Een bestand is geen directory:
        assert_eq!(fs.write_file("/file/sub", b"y"), Err(FsError::NotADirectory));
        assert_eq!(fs.create_dir("/file"), Err(FsError::AlreadyExists));
    }

    #[test]
    fn root_bestaat_altijd() {
        let fs = fresh();
        assert!(fs.exists("/"));
        assert_eq!(fs.metadata("/").unwrap().kind, EntryKind::Directory);
    }
}
