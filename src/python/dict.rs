//! Hash tables: `dict` (insertion order) and `set` (CPython's own order).
//!
//! A `dict` keeps its entries in insertion order, with an index of
//! positions beside them -- the "compact dict" CPython has had since 3.6;
//! its iteration order is therefore the language's, whatever the keys.
//!
//! A `set` has no order the language promises, but programs print sets,
//! and their output is what a user compares. So `SetTable` is CPython's
//! table (`Objects/setobject.c`): the same sizes, the same nine linear
//! probes before the perturbed jump, the same growth (to four times the
//! used count), the same hash functions for numbers and tuples
//! (`hash.rs`). A set of ints prints in CPython's order. Strings hash
//! differently (CPython randomizes them at each start anyway).
//!
//! Neither table knows how to compare keys: user classes define `__eq__`,
//! which runs Python code. The caller passes the comparison (`eq`), which
//! may fail; equal identity is checked first, without calling it.

use alloc::vec::Vec;

use super::value::Value;

/// What comparing two keys may give: equal, or an exception.
pub type Eq<'a, E> = &'a mut dyn FnMut(&Value, &Value) -> Result<bool, E>;

#[derive(Clone)]
pub struct Entry {
    pub hash: i64,
    pub key: Value,
    pub value: Value,
}

#[derive(Clone, Default)]
pub struct Dict {
    entries: Vec<Option<Entry>>,
    /// Open addressing: index into `entries`, or EMPTY / DELETED.
    index: Vec<i32>,
    used: usize,
}

const EMPTY: i32 = -1;
const DELETED: i32 = -2;

/// SipHash-1-3 with a zero key: CPython's string and bytes hash when
/// `PYTHONHASHSEED=0` (its hash is random at each start otherwise). With
/// the same hashes, a set of strings iterates in CPython's order.
pub fn siphash13(data: &[u8]) -> u64 {
    let (k0, k1) = (0u64, 0u64);
    let mut v0 = k0 ^ 0x736f_6d65_7073_6575;
    let mut v1 = k1 ^ 0x646f_7261_6e64_6f6d;
    let mut v2 = k0 ^ 0x6c79_6765_6e65_7261;
    let mut v3 = k1 ^ 0x7465_6462_7974_6573;
    let round = |v0: &mut u64, v1: &mut u64, v2: &mut u64, v3: &mut u64| {
        *v0 = v0.wrapping_add(*v1);
        *v2 = v2.wrapping_add(*v3);
        *v1 = v1.rotate_left(13) ^ *v0;
        *v3 = v3.rotate_left(16) ^ *v2;
        *v0 = v0.rotate_left(32);
        *v2 = v2.wrapping_add(*v1);
        *v0 = v0.wrapping_add(*v3);
        *v1 = v1.rotate_left(17) ^ *v2;
        *v3 = v3.rotate_left(21) ^ *v0;
        *v2 = v2.rotate_left(32);
    };
    let mut chunks = data.chunks_exact(8);
    for chunk in &mut chunks {
        let mut word = [0u8; 8];
        word.copy_from_slice(chunk);
        let m = u64::from_le_bytes(word);
        v3 ^= m;
        round(&mut v0, &mut v1, &mut v2, &mut v3);
        v0 ^= m;
    }
    let mut last = [0u8; 8];
    let rest = chunks.remainder();
    last[..rest.len()].copy_from_slice(rest);
    let b = ((data.len() as u64) << 56) | u64::from_le_bytes(last);
    v3 ^= b;
    round(&mut v0, &mut v1, &mut v2, &mut v3);
    v0 ^= b;
    v2 ^= 0xff;
    for _ in 0..3 {
        round(&mut v0, &mut v1, &mut v2, &mut v3);
    }
    (v0 ^ v1) ^ (v2 ^ v3)
}

fn finish_hash(h: u64) -> i64 {
    let h = h as i64;
    if h == -1 { -2 } else { h }
}

/// Hash of bytes, as CPython's.
pub fn bytes_hash(data: &[u8]) -> i64 {
    if data.is_empty() {
        return 0;
    }
    finish_hash(siphash13(data))
}

/// Hash of a string, as CPython's: SipHash over its code points stored
/// one, two or four bytes each, as CPython stores the string.
pub fn str_hash(text: &str) -> i64 {
    if text.is_empty() {
        return 0;
    }
    if text.is_ascii() {
        return finish_hash(siphash13(text.as_bytes()));
    }
    let widest = text.chars().map(|c| c as u32).max().unwrap_or(0);
    let mut units = Vec::with_capacity(text.len() * 2);
    for c in text.chars() {
        let code = c as u32;
        if widest < 0x100 {
            units.push(code as u8);
        } else if widest < 0x10000 {
            units.extend_from_slice(&(code as u16).to_le_bytes());
        } else {
            units.extend_from_slice(&code.to_le_bytes());
        }
    }
    finish_hash(siphash13(&units))
}

impl Dict {
    pub fn new() -> Dict {
        Dict { entries: Vec::new(), index: Vec::new(), used: 0 }
    }

    pub fn len(&self) -> usize {
        self.used
    }

    pub fn is_empty(&self) -> bool {
        self.used == 0
    }

    fn mask(&self) -> usize {
        self.index.len().wrapping_sub(1)
    }

    fn rebuild(&mut self, size: usize) {
        // Compacts the entries, then indexes them again.
        self.entries.retain(|e| e.is_some());
        let mut size = size.max(8);
        while size * 2 < self.entries.len() * 3 {
            size *= 2;
        }
        self.index = alloc::vec![EMPTY; size.next_power_of_two()];
        let mask = self.mask();
        for (position, entry) in self.entries.iter().enumerate() {
            let hash = entry.as_ref().map_or(0, |e| e.hash) as u64;
            let mut i = hash as usize & mask;
            let mut perturb = hash;
            while self.index[i] != EMPTY {
                perturb >>= 5;
                i = (i.wrapping_mul(5).wrapping_add(1).wrapping_add(perturb as usize)) & mask;
            }
            self.index[i] = position as i32;
        }
    }

    /// The slot of `key` in the index, and its entry position if present.
    fn probe<E>(&self, hash: i64, key: &Value, eq: Eq<E>) -> Result<(usize, Option<usize>), E> {
        let mask = self.mask();
        let mut i = hash as u64 as usize & mask;
        let mut perturb = hash as u64;
        let mut free = None;
        loop {
            match self.index[i] {
                EMPTY => return Ok((free.unwrap_or(i), None)),
                DELETED => {
                    if free.is_none() {
                        free = Some(i);
                    }
                }
                position => {
                    if let Some(entry) = &self.entries[position as usize] {
                        if entry.hash == hash && (entry.key.is(key) || eq(&entry.key, key)?) {
                            return Ok((i, Some(position as usize)));
                        }
                    }
                }
            }
            perturb >>= 5;
            i = (i.wrapping_mul(5).wrapping_add(1).wrapping_add(perturb as usize)) & mask;
        }
    }

    pub fn get<E>(&self, hash: i64, key: &Value, eq: Eq<E>) -> Result<Option<Value>, E> {
        if self.used == 0 {
            return Ok(None);
        }
        let (_, found) = self.probe(hash, key, eq)?;
        Ok(found.and_then(|p| self.entries[p].as_ref().map(|e| e.value.clone())))
    }

    pub fn contains<E>(&self, hash: i64, key: &Value, eq: Eq<E>) -> Result<bool, E> {
        if self.used == 0 {
            return Ok(false);
        }
        Ok(self.probe(hash, key, eq)?.1.is_some())
    }

    /// Sets `key` to `value`; returns the former value.
    pub fn insert<E>(&mut self, hash: i64, key: Value, value: Value, eq: Eq<E>) -> Result<Option<Value>, E> {
        if self.index.is_empty() || (self.entries.len() + 1) * 3 >= self.index.len() * 2 {
            let size = self.index.len().max(8) * if self.used * 3 >= self.index.len() { 2 } else { 1 };
            self.rebuild(size);
        }
        let (slot, found) = self.probe(hash, &key, eq)?;
        if let Some(position) = found {
            if let Some(entry) = &mut self.entries[position] {
                return Ok(Some(core::mem::replace(&mut entry.value, value)));
            }
        }
        self.index[slot] = self.entries.len() as i32;
        self.entries.push(Some(Entry { hash, key, value }));
        self.used += 1;
        Ok(None)
    }

    pub fn remove<E>(&mut self, hash: i64, key: &Value, eq: Eq<E>) -> Result<Option<Value>, E> {
        if self.used == 0 {
            return Ok(None);
        }
        let (slot, found) = self.probe(hash, key, eq)?;
        let Some(position) = found else {
            return Ok(None);
        };
        self.index[slot] = DELETED;
        self.used -= 1;
        let entry = self.entries[position].take();
        Ok(entry.map(|e| e.value))
    }

    /// The live entries, in order.
    pub fn iter(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter_map(|e| e.as_ref())
    }

    /// The entry at a raw position (iterators walk positions, so that a
    /// dict changed under them is noticed rather than misread).
    pub fn entry_at(&self, position: usize) -> Option<&Entry> {
        self.entries.get(position).and_then(|e| e.as_ref())
    }

    pub fn raw_len(&self) -> usize {
        self.entries.len()
    }

    pub fn keys(&self) -> Vec<Value> {
        self.iter().map(|e| e.key.clone()).collect()
    }

    pub fn values(&self) -> Vec<Value> {
        self.iter().map(|e| e.value.clone()).collect()
    }

    pub fn clear(&mut self) {
        *self = Dict::new();
    }

    /// Removes and returns the last entry (`popitem`).
    pub fn pop_last(&mut self) -> Option<Entry> {
        while let Some(last) = self.entries.pop() {
            if let Some(entry) = last {
                self.used -= 1;
                self.rebuild(self.index.len());
                return Some(entry);
            }
        }
        None
    }

    // --------------------------------------------------------
    //  String keys: attribute dictionaries, globals
    // --------------------------------------------------------

    fn str_eq(a: &Value, b: &Value) -> Result<bool, ()> {
        Ok(match (a, b) {
            (Value::Str(x), Value::Str(y)) => x == y,
            _ => false,
        })
    }

    pub fn get_str(&self, name: &str) -> Option<Value> {
        let key = Value::str(name);
        self.get(str_hash(name), &key, &mut Dict::str_eq).ok().flatten()
    }

    pub fn set_str(&mut self, name: &str, value: Value) {
        let key = Value::str(name);
        let _ = self.insert(str_hash(name), key, value, &mut Dict::str_eq);
    }

    pub fn remove_str(&mut self, name: &str) -> Option<Value> {
        let key = Value::str(name);
        self.remove(str_hash(name), &key, &mut Dict::str_eq).ok().flatten()
    }
}

// ============================================================
//  CPython's set table
// ============================================================

#[derive(Clone)]
enum Slot {
    Empty,
    Dummy,
    Active(i64, Value),
}

#[derive(Clone)]
pub struct SetTable {
    table: Vec<Slot>,
    fill: usize,
    used: usize,
    /// Where `pop` looks next.
    finger: usize,
}

const MINSIZE: usize = 8;
const LINEAR_PROBES: usize = 9;
const PERTURB_SHIFT: u32 = 5;

impl Default for SetTable {
    fn default() -> Self {
        SetTable::new()
    }
}

impl SetTable {
    pub fn new() -> SetTable {
        SetTable { table: alloc::vec![Slot::Empty; MINSIZE], fill: 0, used: 0, finger: 0 }
    }

    pub fn len(&self) -> usize {
        self.used
    }

    pub fn is_empty(&self) -> bool {
        self.used == 0
    }

    fn mask(&self) -> usize {
        self.table.len() - 1
    }

    /// `set_add_entry`.
    pub fn add<E>(&mut self, hash: i64, key: Value, eq: Eq<E>) -> Result<bool, E> {
        let mask = self.mask();
        let mut i = hash as u64 as usize & mask;
        let mut perturb = hash as u64;
        let mut free: Option<usize> = None;
        loop {
            let probes = if i + LINEAR_PROBES <= mask { LINEAR_PROBES } else { 0 };
            let mut j = i;
            for _ in 0..=probes {
                match &self.table[j] {
                    Slot::Empty => {
                        match free {
                            Some(slot) => {
                                self.table[slot] = Slot::Active(hash, key);
                                self.used += 1;
                            }
                            None => {
                                self.table[j] = Slot::Active(hash, key);
                                self.fill += 1;
                                self.used += 1;
                                if self.fill * 5 >= mask * 3 {
                                    self.resize(if self.used > 50000 { self.used * 2 } else { self.used * 4 });
                                }
                            }
                        }
                        return Ok(true);
                    }
                    Slot::Active(h, k) => {
                        if *h == hash && (k.is(&key) || eq(k, &key)?) {
                            return Ok(false);
                        }
                    }
                    Slot::Dummy => {
                        if free.is_none() {
                            free = Some(j);
                        }
                    }
                }
                j += 1;
            }
            perturb >>= PERTURB_SHIFT;
            i = (i.wrapping_mul(5).wrapping_add(1).wrapping_add(perturb as usize)) & mask;
        }
    }

    /// `set_insert_clean`: into a table known to hold neither the key nor
    /// dummies.
    fn insert_clean(table: &mut [Slot], hash: i64, key: Value) {
        let mask = table.len() - 1;
        let mut i = hash as u64 as usize & mask;
        let mut perturb = hash as u64;
        loop {
            if matches!(table[i], Slot::Empty) {
                table[i] = Slot::Active(hash, key);
                return;
            }
            if i + LINEAR_PROBES <= mask {
                for j in 1..=LINEAR_PROBES {
                    if matches!(table[i + j], Slot::Empty) {
                        table[i + j] = Slot::Active(hash, key);
                        return;
                    }
                }
            }
            perturb >>= PERTURB_SHIFT;
            i = (i.wrapping_mul(5).wrapping_add(1).wrapping_add(perturb as usize)) & mask;
        }
    }

    /// `set_table_resize`.
    fn resize(&mut self, minused: usize) {
        let mut size = MINSIZE;
        while size <= minused {
            size <<= 1;
        }
        let old = core::mem::replace(&mut self.table, alloc::vec![Slot::Empty; size]);
        self.fill = self.used;
        for slot in old {
            if let Slot::Active(hash, key) = slot {
                SetTable::insert_clean(&mut self.table, hash, key);
            }
        }
    }

    fn find<E>(&self, hash: i64, key: &Value, eq: Eq<E>) -> Result<Option<usize>, E> {
        let mask = self.mask();
        let mut i = hash as u64 as usize & mask;
        let mut perturb = hash as u64;
        loop {
            let probes = if i + LINEAR_PROBES <= mask { LINEAR_PROBES } else { 0 };
            let mut j = i;
            for _ in 0..=probes {
                match &self.table[j] {
                    Slot::Empty => return Ok(None),
                    Slot::Active(h, k) => {
                        if *h == hash && (k.is(key) || eq(k, key)?) {
                            return Ok(Some(j));
                        }
                    }
                    Slot::Dummy => {}
                }
                j += 1;
            }
            perturb >>= PERTURB_SHIFT;
            i = (i.wrapping_mul(5).wrapping_add(1).wrapping_add(perturb as usize)) & mask;
        }
    }

    pub fn contains<E>(&self, hash: i64, key: &Value, eq: Eq<E>) -> Result<bool, E> {
        Ok(self.find(hash, key, eq)?.is_some())
    }

    /// `set_discard_entry`: the slot becomes a dummy.
    pub fn discard<E>(&mut self, hash: i64, key: &Value, eq: Eq<E>) -> Result<bool, E> {
        match self.find(hash, key, eq)? {
            Some(j) => {
                self.table[j] = Slot::Dummy;
                self.used -= 1;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// The members with their hashes, in table order.
    pub fn entries(&self) -> Vec<(i64, Value)> {
        self.table
            .iter()
            .filter_map(|s| match s {
                Slot::Active(h, k) => Some((*h, k.clone())),
                _ => None,
            })
            .collect()
    }

    pub fn keys(&self) -> Vec<Value> {
        self.entries().into_iter().map(|(_, k)| k).collect()
    }

    /// `set_pop`: from the finger on.
    pub fn pop(&mut self) -> Option<Value> {
        if self.used == 0 {
            return None;
        }
        let mask = self.mask();
        let mut i = self.finger & mask;
        loop {
            if let Slot::Active(..) = self.table[i] {
                break;
            }
            i = (i + 1) & mask;
        }
        let slot = core::mem::replace(&mut self.table[i], Slot::Dummy);
        self.used -= 1;
        self.finger = i + 1;
        match slot {
            Slot::Active(_, key) => Some(key),
            _ => None,
        }
    }

    pub fn clear(&mut self) {
        *self = SetTable::new();
    }

    /// The resize `set_update_internal` does before adding `extra` keys
    /// from a dict.
    pub fn presize(&mut self, extra: usize) {
        if (self.fill + extra) * 5 >= self.mask() * 3 {
            self.resize((self.used + extra) * 2);
        }
    }

    /// After discards: more than a quarter of dummies are resized away
    /// (`set_difference_update_internal`).
    pub fn drop_dummies(&mut self) {
        if self.fill - self.used > self.mask() / 4 {
            self.resize(if self.used > 50000 { self.used * 2 } else { self.used * 4 });
        }
    }

    /// `set_merge` from another set: a straight copy of its table when
    /// this one is empty and the sizes match, as CPython does.
    pub fn merge<E>(&mut self, other: &SetTable, eq: Eq<E>) -> Result<(), E> {
        if other.used == 0 {
            return Ok(());
        }
        if (self.fill + other.used) * 5 >= self.mask() * 3 {
            self.resize((self.used + other.used) * 2);
        }
        if self.fill == 0 && self.mask() == other.mask() && other.fill == other.used {
            self.table = other.table.clone();
            self.fill = other.fill;
            self.used = other.used;
            return Ok(());
        }
        if self.fill == 0 {
            let mut table = alloc::vec![Slot::Empty; self.table.len()];
            for slot in &other.table {
                if let Slot::Active(hash, key) = slot {
                    SetTable::insert_clean(&mut table, *hash, key.clone());
                }
            }
            self.table = table;
            self.fill = other.used;
            self.used = other.used;
            return Ok(());
        }
        for slot in &other.table {
            if let Slot::Active(hash, key) = slot {
                self.add(*hash, key.clone(), eq)?;
            }
        }
        Ok(())
    }
}
