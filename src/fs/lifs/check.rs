//! The check of a LIFS volume (`fs::check_all`, and the power-cut trial
//! of the self-test): everything the structures say must agree.
//!
//! - every record is reached from the root, and names as many entries as
//!   its `links`; a directory holds as many entries as its `entries`;
//! - the reverse index holds exactly (node, directory, count) of the
//!   entries found;
//! - no block is two things: a tree block, a chain block and a data run
//!   never overlap, except data runs with data runs -- the sharing of a
//!   clone, which the refcount tree must state exactly;
//! - every block in use is set in the bitmap, and the bitmap sets nothing
//!   else but the runs waiting for the commit; the free count is the
//!   bitmap's.
//!
//! Runs rather than blocks: a 1 GiB file is one run here, not 262 144
//! entries.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use super::btree::{self, Key};
use super::format::{get_u32, get_u64, BLOCK, ROOT};
use super::Volume;
use crate::fs::FsError;

fn fail(what: &str) -> FsError {
    crate::serial_println!("[lifs] verification : {}", what);
    FsError::Corrupt
}

/// Sorted (start, end, count) segments of constant count, from runs.
fn coverage(runs: &[(u64, u64)]) -> Vec<(u64, u64, u32)> {
    let mut events: Vec<(u64, i64)> = Vec::with_capacity(runs.len() * 2);
    for (start, len) in runs {
        if *len > 0 {
            events.push((*start, 1));
            events.push((start + len, -1));
        }
    }
    events.sort_unstable();
    let mut out: Vec<(u64, u64, u32)> = Vec::new();
    let mut depth: i64 = 0;
    let mut at = 0;
    for (position, delta) in events {
        if position > at && depth > 0 {
            push_merged(&mut out, at, position, depth as u32);
        }
        depth += delta;
        at = position;
    }
    out
}

fn push_merged(out: &mut Vec<(u64, u64, u32)>, start: u64, end: u64, count: u32) {
    if let Some(last) = out.last_mut() {
        if last.1 == start && last.2 == count {
            last.1 = end;
            return;
        }
    }
    out.push((start, end, count));
}

impl Volume {
    pub fn check_volume(&mut self) -> Result<(usize, usize), FsError> {
        let data_start = self.sb.data_start();
        let total = self.sb.total_blocks;
        let records = self.all_records()?;
        if records.len() as u64 != self.sb.node_count {
            return Err(fail("compte de noeuds du superbloc faux"));
        }
        let by_id: BTreeMap<u64, usize> = records.iter().enumerate().map(|(i, r)| (r.id, i)).collect();
        if !by_id.contains_key(&ROOT) || !records[by_id[&ROOT]].is_dir() {
            return Err(fail("pas de dossier racine"));
        }

        // --- the tree of names ---
        let mut named: BTreeMap<u64, u32> = BTreeMap::new();
        let mut reverse: BTreeMap<(u64, u64), u64> = BTreeMap::new();
        let mut meta: Vec<u64> = Vec::new();
        let mut reached: BTreeMap<u64, ()> = BTreeMap::new();
        let mut queue = alloc::vec![ROOT];
        reached.insert(ROOT, ());
        named.insert(ROOT, 1);
        while let Some(dir_id) = queue.pop() {
            let dir = records[by_id[&dir_id]].clone();
            let entries = self.entries_of(&dir)?;
            if entries.len() as u64 != dir.entries {
                return Err(fail("nombre d'entrees d'un dossier faux"));
            }
            btree::blocks(self, dir.tree, &mut meta)?;
            for entry in entries {
                let Some(&index) = by_id.get(&entry.target) else {
                    return Err(fail("entree vers un noeud absent"));
                };
                *named.entry(entry.target).or_insert(0) += 1;
                *reverse.entry((entry.target, dir_id)).or_insert(0) += 1;
                if reached.insert(entry.target, ()).is_none() && records[index].is_dir() {
                    queue.push(entry.target);
                } else if records[index].is_dir() {
                    return Err(fail("dossier nomme deux fois"));
                }
            }
        }
        for record in records.iter() {
            let count = named.get(&record.id).copied().unwrap_or(0);
            if count == 0 {
                return Err(fail("noeud inaccessible depuis la racine"));
            }
            if count != record.links {
                return Err(fail("compteur de liens d'un noeud faux"));
            }
        }
        let root = self.sb.reverse_root;
        let stored: Vec<((u64, u64), u64)> =
            btree::range(self, root, Key::MIN, Key::MAX)?.iter().map(|(k, v)| ((k.hi, k.lo), get_u64(v, 0))).collect();
        if stored.len() != reverse.len() || stored.iter().any(|(k, v)| reverse.get(k) != Some(v)) {
            return Err(fail("index inverse different des entrees"));
        }

        // --- blocks ---
        let node_root = self.sb.node_root;
        let refcount_root = self.sb.refcount_root;
        let reverse_root = self.sb.reverse_root;
        btree::blocks(self, node_root, &mut meta)?;
        btree::blocks(self, refcount_root, &mut meta)?;
        btree::blocks(self, reverse_root, &mut meta)?;
        let mut data: Vec<(u64, u64)> = Vec::new();
        for record in records.iter().filter(|r| !r.is_dir()) {
            let extents = self.extents(record)?;
            let mut all = Vec::new();
            self.content_blocks(record, &mut all)?;
            for e in extents.iter() {
                if e.len == 0 || e.phys < data_start || e.phys + e.len > total {
                    return Err(fail("etendue hors de la zone de donnees"));
                }
                if e.logical * BLOCK as u64 >= record.size.div_ceil(BLOCK as u64) * BLOCK as u64 {
                    return Err(fail("etendue au-dela de la taille du fichier"));
                }
                data.push((e.phys, e.len));
            }
            meta.extend(all[extents.len()..].iter().map(|(b, _)| *b));
        }
        let mut meta_runs: Vec<(u64, u64)> = meta.iter().map(|b| (*b, 1)).collect();
        for b in meta.iter() {
            if *b < data_start || *b >= total {
                return Err(fail("bloc d'arbre hors de la zone de donnees"));
            }
        }
        let shared = coverage(&data);
        let metadata = coverage(&meta_runs);
        if metadata.iter().any(|(_, _, c)| *c > 1) {
            return Err(fail("bloc d'arbre designe deux fois"));
        }
        // Metadata and data never overlap.
        meta_runs.extend(data.iter().copied());
        let everything = coverage(&meta_runs);
        let data_total: u64 = shared.iter().map(|(s, e, _)| e - s).sum();
        let meta_total: u64 = metadata.iter().map(|(s, e, _)| e - s).sum();
        let used_total: u64 = everything.iter().map(|(s, e, _)| e - s).sum();
        if used_total != data_total + meta_total {
            return Err(fail("bloc a la fois arbre et donnees"));
        }

        // The refcount tree: exactly the segments counted twice or more.
        let mut expected: Vec<(u64, u64, u32)> = Vec::new();
        for (s, e, c) in shared.iter().filter(|(_, _, c)| *c >= 2) {
            push_merged(&mut expected, *s, *e, *c);
        }
        let mut found: Vec<(u64, u64, u32)> = Vec::new();
        for (k, v) in btree::range(self, refcount_root, Key::MIN, Key::MAX)? {
            let (len, refs) = (get_u64(&v, 0), get_u32(&v, 8));
            if refs < 2 || len == 0 {
                return Err(fail("entree de comptes de references inutile"));
            }
            if found.last().is_some_and(|last| last.1 > k.hi) {
                return Err(fail("comptes de references qui se chevauchent"));
            }
            push_merged(&mut found, k.hi, k.hi + len, refs);
        }
        if expected != found {
            return Err(fail("comptes de references differents des partages"));
        }

        // The bitmap: in use, or waiting for the commit -- nothing else.
        let pending: Vec<(u64, u64)> = self.pending.iter().map(|(s, l)| (*s, *l)).collect();
        let mut set_expected = everything.clone();
        for (s, e, _) in coverage(&pending) {
            if everything.iter().any(|(a, b, _)| *a < e && s < *b) {
                return Err(fail("bloc libere encore designe"));
            }
            set_expected.push((s, e, 1));
        }
        set_expected.sort_unstable();
        let mut set_count = 0u64;
        let mut cursor = 0usize;
        let mut image = [0u8; BLOCK];
        let mut loaded = u64::MAX;
        let per = BLOCK as u64 * 8;
        for block in data_start..total {
            let map = self.sb.bitmap_start + block / per;
            if map != loaded {
                self.store.read(map, &mut image)?;
                loaded = map;
            }
            let bit = block % per;
            let set = image[(bit / 8) as usize] & (1 << (bit % 8)) != 0;
            while cursor < set_expected.len() && set_expected[cursor].1 <= block {
                cursor += 1;
            }
            let wanted = cursor < set_expected.len() && set_expected[cursor].0 <= block;
            if set != wanted {
                return Err(fail(if set { "bloc marque occupe que rien ne designe" } else { "bloc designe marque libre" }));
            }
            if set {
                set_count += 1;
            }
        }
        if self.sb.free_blocks + set_count != total - data_start {
            return Err(fail("compte de blocs libres du superbloc faux"));
        }
        // The metadata of the reserved zone is set too.
        for block in 0..data_start {
            if !self.used(block)? {
                return Err(fail("zone reservee marquee libre"));
            }
        }
        Ok((records.len(), used_total as usize))
    }
}
