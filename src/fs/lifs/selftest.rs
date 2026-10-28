//! The self-test of LIFS (14.6.1, 14.6.2), on volumes of its own -- never
//! on a mounted one.
//!
//! The medium is a **sparse disk**: it answers for 4 GiB and keeps only
//! the blocks written, so a 1 GiB file costs its extents, not a gibibyte
//! of heap. It counts its writes and can be told to drop every write past
//! the n-th: that is the power cut. Everything the cache had not handed
//! over by then is lost, as on a real disk.
//!
//! What is measured is counted, not timed: the blocks a clone puts in the
//! journal, the writes the medium receives, the free blocks before and
//! after.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::format::{Extent, BLOCK};
use super::Volume;
use crate::fs::block::{BlockDevice, BlockError, MediumKind};
use crate::fs::cache::BlockCache;
use crate::fs::filesystem::Filesystem;
use crate::fs::perm::{Credentials, Mode};
use crate::fs::FsError;
use crate::vga::SpinLock;

// ============================================================
//  The sparse disk
// ============================================================

type Blocks = BTreeMap<u64, Box<[u8; BLOCK]>>;

struct Sparse {
    blocks: Blocks,
    count: u64,
    writes: u64,
    /// Writes past this one are dropped: the power is cut.
    limit: u64,
}

#[derive(Clone)]
struct SparseDisk(Arc<SpinLock<Sparse>>);

impl SparseDisk {
    fn new(count: u64) -> SparseDisk {
        SparseDisk::from(Blocks::new(), count)
    }

    fn from(blocks: Blocks, count: u64) -> SparseDisk {
        SparseDisk(Arc::new(SpinLock::new(Sparse { blocks, count, writes: 0, limit: u64::MAX })))
    }

    fn writes(&self) -> u64 {
        self.0.lock().writes
    }

    fn snapshot(&self) -> Blocks {
        self.0.lock().blocks.clone()
    }

    fn cut_after(&self, writes: u64) {
        let mut disk = self.0.lock();
        disk.limit = disk.writes + writes;
    }

    fn restore_power(&self) {
        self.0.lock().limit = u64::MAX;
    }

    fn cache(&self) -> BlockCache {
        BlockCache::new(Box::new(self.clone()))
    }
}

impl BlockDevice for SparseDisk {
    fn name(&self) -> String {
        String::from("disque creux d'essai")
    }

    fn kind(&self) -> MediumKind {
        MediumKind::Ram
    }

    fn block_count(&self) -> u64 {
        self.0.lock().count
    }

    fn read_block(&mut self, index: u64, buffer: &mut [u8]) -> Result<(), BlockError> {
        let disk = self.0.lock();
        if buffer.len() != BLOCK {
            return Err(BlockError::BadBufferSize);
        }
        if index >= disk.count {
            return Err(BlockError::OutOfRange);
        }
        match disk.blocks.get(&index) {
            Some(block) => buffer.copy_from_slice(&block[..]),
            None => buffer.fill(0),
        }
        Ok(())
    }

    fn write_block(&mut self, index: u64, buffer: &[u8]) -> Result<(), BlockError> {
        let mut disk = self.0.lock();
        if buffer.len() != BLOCK {
            return Err(BlockError::BadBufferSize);
        }
        if index >= disk.count {
            return Err(BlockError::OutOfRange);
        }
        disk.writes += 1;
        if disk.writes > disk.limit {
            return Ok(());
        }
        if buffer.iter().all(|b| *b == 0) {
            disk.blocks.remove(&index);
        } else {
            let mut block = Box::new([0u8; BLOCK]);
            block.copy_from_slice(buffer);
            disk.blocks.insert(index, block);
        }
        Ok(())
    }
}

// ============================================================
//  Report
// ============================================================

struct Report {
    passed: usize,
    failed: usize,
}

impl Report {
    fn check(&mut self, label: &str, condition: bool) {
        if condition {
            self.passed += 1;
        } else {
            self.failed += 1;
            crate::serial_println!("[lifs] ECHEC : {}", label);
        }
    }

    fn ok<T>(&mut self, label: &str, result: Result<T, FsError>) -> Option<T> {
        match result {
            Ok(value) => {
                self.passed += 1;
                Some(value)
            }
            Err(error) => {
                self.failed += 1;
                crate::serial_println!("[lifs] ECHEC : {} - {} {}", label, error.message(), error.detail());
                None
            }
        }
    }
}

const KERNEL: Credentials = Credentials::kernel();
const GIB: u64 = 1 << 30;

fn byte_at(volume: &mut Volume, path: &str, offset: u64) -> Option<u8> {
    let node = Filesystem::resolve(volume, path, KERNEL).ok()?;
    let mut byte = [0u8; 1];
    match Filesystem::read_range(volume, &node, offset, &mut byte) {
        Ok(1) => Some(byte[0]),
        _ => None,
    }
}

fn extent_count(volume: &mut Volume, path: &str) -> usize {
    let Ok(node) = Filesystem::resolve(volume, path, KERNEL) else {
        return usize::MAX;
    };
    match volume.load_record(node.id).and_then(|r| volume.extents(&r)) {
        Ok(extents) => extents.len(),
        Err(_) => usize::MAX,
    }
}

/// Gives file `id` `blocks` blocks of the free space without writing them:
/// the 1 GiB of the trial is allocated, not filled (the sparse disk reads
/// zeros where nothing was written).
fn allocate(volume: &mut Volume, id: u64, blocks: u64) -> Result<(), FsError> {
    let mut record = volume.load_record(id)?;
    let mut extents = volume.extents(&record)?;
    let mut logical = extents.last().map(|e| e.logical + e.len).unwrap_or(0);
    let mut left = blocks;
    while left > 0 {
        let (phys, len) = volume.alloc_run(left)?;
        extents.push(Extent { logical, phys, len });
        logical += len;
        left -= len;
    }
    volume.set_extents(&mut record, &extents)?;
    record.size = logical * BLOCK as u64;
    volume.save_record(&record)
}

// ============================================================
//  14.6.1 -- clone, copy on write, Tag, destruction
// ============================================================

fn clone_and_tags(report: &mut Report, now: u64) {
    crate::serial_println!("[lifs] --- clone CoW, lien Tag, destruction (14.6.1) ---");
    let disk = SparseDisk::new(1 << 20);
    let Some(mut volume) = report.ok("formatage d'un volume LIFS de 4 Gio", Volume::format(disk.cache(), "essai-lifs", 0, now)) else {
        return;
    };
    let v = &mut volume;
    report.ok("volume neuf coherent", v.check_volume());
    let file = Mode::shared_file();
    let dir = Mode::shared_dir();
    let Some(a) = report.ok("dossier /a", Filesystem::create_path(v, "/a", KERNEL, dir, now)) else {
        return;
    };
    report.ok("validation", Filesystem::flush(v));
    let free_base = v.sb.free_blocks;

    // --- the 1 GiB file ---
    let Some(big) = report.ok("fichier /grand", Filesystem::write_file(v, "/grand", b"", KERNEL, file, now)) else {
        return;
    };
    report.ok("1 Gio alloue a /grand", allocate(v, big, GIB / BLOCK as u64));
    let mut node = v.read_node(big).unwrap_or_else(|_| crate::fs::format::Node::empty(big));
    report.ok("marqueur a 512 Mio", Filesystem::write_range(v, &mut node, GIB / 2, b"O", now));
    report.ok("validation", Filesystem::flush(v));
    let big_extents = extent_count(v, "/grand");
    report.check("1 Gio en peu d'etendues", big_extents <= 7);

    let writes = disk.writes();
    let free_before_clone = v.sb.free_blocks;
    report.ok("clone de /grand", Filesystem::copy(v, "/grand", "/clone", KERNEL, now));
    let journal_big = v.store_mut().pending();
    report.ok("validation", Filesystem::flush(v));
    let writes_big = disk.writes() - writes;
    let clone_cost = free_before_clone.saturating_sub(v.sb.free_blocks);

    let small: Vec<u8> = (0..BLOCK).map(|i| i as u8).collect();
    report.ok("fichier /petit de 4 Kio", Filesystem::write_file(v, "/petit", &small, KERNEL, file, now));
    report.ok("validation", Filesystem::flush(v));
    let writes = disk.writes();
    report.ok("clone de /petit", Filesystem::copy(v, "/petit", "/petit2", KERNEL, now));
    let journal_small = v.store_mut().pending();
    report.ok("validation", Filesystem::flush(v));
    let writes_small = disk.writes() - writes;
    crate::serial_println!(
        "[lifs]   clone de 1 Gio : {} bloc(s) de metadonnees, {} ecriture(s) disque ; clone de 4 Kio : {} bloc(s), {} ecriture(s)",
        journal_big,
        writes_big,
        journal_small,
        writes_small
    );
    report.check("clone de 1 Gio en temps constant (blocs ecrits)", journal_big <= journal_small + 2 && writes_big <= writes_small + 6);
    report.check("clone : aucun bloc de donnees consomme", clone_cost <= 4);
    report.check("le clone lit le contenu de l'original", byte_at(v, "/clone", GIB / 2) == Some(b'O'));
    report.check("/petit2 identique a /petit", Filesystem::read_file(v, "/petit2", KERNEL).ok() == Some(small.clone()));
    report.ok("coherence apres les clones", v.check_volume());

    // --- one byte into the clone: one block copied ---
    let free = v.sb.free_blocks;
    let Ok(mut clone) = Filesystem::resolve(v, "/clone", KERNEL) else {
        report.check("/clone resolu", false);
        return;
    };
    report.ok("un octet ecrit dans le clone", Filesystem::write_range(v, &mut clone, GIB / 2, b"C", now));
    report.ok("validation", Filesystem::flush(v));
    let used = free - v.sb.free_blocks;
    crate::serial_println!(
        "[lifs]   octet ecrit dans le clone : {} bloc(s) pris, etendues du clone {} (original {})",
        used,
        extent_count(v, "/clone"),
        extent_count(v, "/grand")
    );
    report.check("un seul bloc de donnees copie", (1..=4).contains(&used));
    report.check("l'original intact", byte_at(v, "/grand", GIB / 2) == Some(b'O'));
    report.check("le clone modifie", byte_at(v, "/clone", GIB / 2) == Some(b'C'));
    report.check("etendues de l'original inchangees", extent_count(v, "/grand") == big_extents);
    // The original protected as well (point 2 of 14.1).
    let Ok(mut original) = Filesystem::resolve(v, "/grand", KERNEL) else {
        return;
    };
    report.ok("un octet ecrit dans l'original", Filesystem::write_range(v, &mut original, 4096, b"X", now));
    report.check("le clone ne voit pas l'ecriture de l'original", byte_at(v, "/clone", 4096) == Some(0));
    report.check("l'original la voit", byte_at(v, "/grand", 4096) == Some(b'X'));
    report.ok("coherence apres copie sur ecriture", v.check_volume());

    // --- Tag ---
    let clone_id = clone.id;
    report.ok("lien Tag /a/etiquette vers le clone", Filesystem::node_tag(v, clone_id, a, "etiquette", KERNEL, now));
    report.check("Tag : meme contenu", byte_at(v, "/a/etiquette", GIB / 2) == Some(b'C'));
    if let Ok(mut tagged) = Filesystem::resolve(v, "/a/etiquette", KERNEL) {
        report.ok("ecriture par le Tag", Filesystem::write_range(v, &mut tagged, 0, b"T", now));
    }
    report.check("Tag : l'ecriture visible par l'autre nom", byte_at(v, "/clone", 0) == Some(b'T'));
    let tags = Filesystem::node_tags(v, clone_id, KERNEL).unwrap_or_default();
    report.check("index inverse : deux dossiers designent le clone", tags.len() == 2);
    report.check("deux liens", v.load_record(clone_id).map(|r| r.links).ok() == Some(2));
    report.check(
        "un dossier ne recoit pas de Tag",
        Filesystem::node_tag(v, a, super::format::ROOT, "boucle", KERNEL, now) == Err(FsError::IsADirectory),
    );
    report.ok("coherence apres le Tag", v.check_volume());

    // --- destruction at the last link ---
    report.ok("retrait de /clone", Filesystem::delete(v, "/clone", KERNEL, now));
    report.check("le noeud survit par son Tag", byte_at(v, "/a/etiquette", GIB / 2) == Some(b'C'));
    report.ok("retrait du Tag", Filesystem::node_unlink(v, a, "etiquette", KERNEL, now));
    report.check("noeud detruit au dernier lien", v.load_record(clone_id).is_err());
    for path in ["/grand", "/petit", "/petit2"] {
        report.ok(path, Filesystem::delete(v, path, KERNEL, now));
    }
    report.ok("validation", Filesystem::flush(v));
    crate::serial_println!("[lifs]   blocs libres : {} au depart, {} apres destruction", free_base, v.sb.free_blocks);
    report.check("blocs libres revenus a l'identique", v.sb.free_blocks == free_base);
    report.ok("coherence finale", v.check_volume());

    // --- remount: what was committed is on the disk ---
    drop(volume);
    match Volume::mount(disk.cache()) {
        Ok(mut again) => {
            report.check("remonte : /a present", Filesystem::stat(&mut again, "/a", KERNEL).is_ok());
            report.check("remonte : blocs libres identiques", again.sb.free_blocks == free_base);
            report.ok("remonte : coherent", again.check_volume());
        }
        Err(error) => report.check(error.message(), false),
    }
}

// ============================================================
//  14.6.2 -- power cut at every write of a clone and a removal
// ============================================================

/// What a file of the power-cut trial must hold.
fn pattern(len: usize, twist: Option<(usize, u8)>) -> Vec<u8> {
    let mut data: Vec<u8> = (0..len).map(|i| (i * 7 + 3) as u8).collect();
    if let Some((at, byte)) = twist {
        data[at] = byte;
    }
    data
}

const CUT_LEN: usize = 10 * BLOCK;

/// The two transactions cut: a clone of /f with one byte changed in it,
/// then the removal of /f.
fn cut_operations(volume: &mut Volume, now: u64) -> Result<(), FsError> {
    Filesystem::copy(volume, "/f", "/g", KERNEL, now)?;
    let mut g = Filesystem::resolve(volume, "/g", KERNEL)?;
    Filesystem::write_range(volume, &mut g, 5000, b"Z", now)?;
    Filesystem::flush(volume)?;
    Filesystem::delete(volume, "/f", KERNEL, now)?;
    Filesystem::flush(volume)
}

/// Which of the three states the volume is in: 0 before the clone, 1
/// between the two transactions, 2 after the removal; `None`: none.
fn cut_state(volume: &mut Volume) -> Option<u8> {
    let f = Filesystem::read_file(volume, "/f", KERNEL).ok();
    let g = Filesystem::read_file(volume, "/g", KERNEL).ok();
    let original = pattern(CUT_LEN, None);
    let changed = pattern(CUT_LEN, Some((5000, b'Z')));
    match (f, g) {
        (Some(f), None) if f == original => Some(0),
        (Some(f), Some(g)) if f == original && g == changed => Some(1),
        (None, Some(g)) if g == changed => Some(2),
        _ => None,
    }
}

fn power_cuts(report: &mut Report, now: u64) {
    crate::serial_println!("[lifs] --- coupure simulee a chaque ecriture d'un clone et d'un retrait (14.6.2) ---");
    let blocks = 8192;
    let disk = SparseDisk::new(blocks);
    let Some(mut volume) = report.ok("formatage du volume d'essai", Volume::format(disk.cache(), "coupure", 0, now)) else {
        return;
    };
    report.ok("fichier /f", Filesystem::write_file(&mut volume, "/f", &pattern(CUT_LEN, None), KERNEL, Mode::shared_file(), now));
    report.ok("validation", Filesystem::flush(&mut volume));
    drop(volume);
    let image = disk.snapshot();

    // The writes of a whole run, uncut: mount, the two transactions.
    let whole = SparseDisk::from(image.clone(), blocks);
    let total = match Volume::mount(whole.cache()) {
        Ok(mut v) => {
            report.ok("les deux transactions sans coupure", cut_operations(&mut v, now));
            report.check("etat final sans coupure", cut_state(&mut v) == Some(2));
            whole.writes()
        }
        Err(error) => {
            report.check(error.message(), false);
            return;
        }
    };

    let mut states = [0usize; 3];
    let mut broken = 0usize;
    let mut replays = 0usize;
    for cut in 0..=total {
        let trial = SparseDisk::from(image.clone(), blocks);
        trial.cut_after(cut);
        if let Ok(mut v) = Volume::mount(trial.cache()) {
            // Errors are expected: the medium goes silent, not the code.
            let _ = cut_operations(&mut v, now);
        }
        trial.restore_power();
        match Volume::mount(trial.cache()) {
            Ok(mut v) => {
                if v.replayed {
                    replays += 1;
                }
                let coherent = v.check_volume().is_ok();
                match (coherent, cut_state(&mut v)) {
                    (true, Some(state)) => states[state as usize] += 1,
                    _ => {
                        broken += 1;
                        crate::serial_println!("[lifs] ECHEC : coupure apres {} ecriture(s) - volume incoherent", cut);
                    }
                }
            }
            Err(error) => {
                broken += 1;
                crate::serial_println!("[lifs] ECHEC : coupure apres {} ecriture(s) - {}", cut, error.message());
            }
        }
    }
    crate::serial_println!(
        "[lifs]   {} coupures : {} avant le clone, {} entre clone et retrait, {} apres le retrait, {} rejeu(x) du journal, {} incoherence(s)",
        total + 1,
        states[0],
        states[1],
        states[2],
        replays,
        broken
    );
    report.check("aucune coupure ne laisse un volume incoherent", broken == 0);
    report.check("les trois etats atteints", states.iter().all(|n| *n > 0));
    report.check("le journal rejoue au moins une fois", replays > 0);
}

// ============================================================
//  15.0.5 -- replacing an entry atomically (the `current` pointer)
// ============================================================

/// Which version `/current` names: 1, 2, or `None` (no readable entry --
/// the state a power cut must never leave).
fn current_version(volume: &mut Volume) -> Option<u8> {
    match Filesystem::read_file(volume, "/current", KERNEL).ok()?.as_slice() {
        b"version 1" => Some(1),
        b"version 2" => Some(2),
        _ => None,
    }
}

/// The switch of phase 15: the new pointer, written aside, takes the
/// place of the old in one step; its temporary name goes.
fn switch_pointer(volume: &mut Volume, now: u64) -> Result<(), FsError> {
    Filesystem::retarget(volume, "/current", "/next", KERNEL, now)?;
    Filesystem::delete(volume, "/next", KERNEL, now)?;
    Filesystem::flush(volume)
}

fn retarget_cuts(report: &mut Report, now: u64) {
    crate::serial_println!("[lifs] --- pointeur de version remplace atomiquement (15.0.5) ---");
    let blocks = 4096;
    let disk = SparseDisk::new(blocks);
    let Some(mut volume) = report.ok("formatage du volume d'essai", Volume::format(disk.cache(), "pointeur", 0, now)) else {
        return;
    };
    report.ok("pointeur actuel", Filesystem::write_file(&mut volume, "/current", b"version 1", KERNEL, Mode::shared_file(), now));
    report.ok("pointeur suivant", Filesystem::write_file(&mut volume, "/next", b"version 2", KERNEL, Mode::shared_file(), now));
    report.ok("dossier", Filesystem::create_path(&mut volume, "/d", KERNEL, Mode::shared_dir(), now));
    report.check(
        "un dossier ne se designe pas ainsi",
        Filesystem::retarget(&mut volume, "/autre", "/d", KERNEL, now) == Err(FsError::IsADirectory),
    );
    report.ok("validation", Filesystem::flush(&mut volume));
    drop(volume);
    let image = disk.snapshot();

    let whole = SparseDisk::from(image.clone(), blocks);
    let total = match Volume::mount(whole.cache()) {
        Ok(mut v) => {
            let free_before = v.sb.free_blocks;
            report.ok("bascule sans coupure", switch_pointer(&mut v, now));
            report.check("le pointeur designe la version 2", current_version(&mut v) == Some(2));
            report.check("le nom temporaire est parti", Filesystem::stat(&mut v, "/next", KERNEL).is_err());
            report.check("l'ancien pointeur est libere avec son dernier lien", v.sb.free_blocks >= free_before);
            whole.writes()
        }
        Err(error) => {
            report.check(error.message(), false);
            return;
        }
    };

    let mut states = [0usize; 2];
    let mut broken = 0usize;
    for cut in 0..=total {
        let trial = SparseDisk::from(image.clone(), blocks);
        trial.cut_after(cut);
        if let Ok(mut v) = Volume::mount(trial.cache()) {
            let _ = switch_pointer(&mut v, now);
        }
        trial.restore_power();
        match Volume::mount(trial.cache()) {
            Ok(mut v) => match (v.check_volume().is_ok(), current_version(&mut v)) {
                (true, Some(version)) => states[version as usize - 1] += 1,
                _ => {
                    broken += 1;
                    crate::serial_println!("[lifs] ECHEC : coupure apres {} ecriture(s) - pointeur illisible", cut);
                }
            },
            Err(error) => {
                broken += 1;
                crate::serial_println!("[lifs] ECHEC : coupure apres {} ecriture(s) - {}", cut, error.message());
            }
        }
    }
    crate::serial_println!(
        "[lifs]   {} coupures pendant la bascule : {} sur l'ancienne version, {} sur la nouvelle, {} sans pointeur",
        total + 1,
        states[0],
        states[1],
        broken
    );
    report.check("une coupure laisse l'ancienne ou la nouvelle version", broken == 0);
    report.check("les deux issues atteintes", states.iter().all(|n| *n > 0));
}

// ============================================================
//  14.0.4 -- writing a GPT, and a LIFS in a partition
// ============================================================

fn partitions(report: &mut Report, now: u64) {
    use crate::fs::partition::{self, write, PartitionDevice, PartitionKind, Table, LIFS_TYPE};
    crate::serial_println!("[lifs] --- table GPT ecrite par le noyau, LIFS dans une partition (14.0.4) ---");
    let disk = SparseDisk::new(1 << 16);
    let mut device: Box<dyn BlockDevice> = Box::new(disk.clone());
    let first = write::create_table(device.as_mut(), LIFS_TYPE, "essai-a", 64 * 2048);
    let Some(first) = report.ok("table GPT creee", first.map_err(|_| FsError::Unsupported)) else {
        return;
    };
    report.check("une table ne s'ecrit pas par-dessus une autre", write::create_table(device.as_mut(), LIFS_TYPE, "x", 0).err() == Some(write::WriteError::NotBlank));
    let second = write::add_partition(device.as_mut(), LIFS_TYPE, "essai-b", 0);
    report.check("partition ajoutee dans l'espace libre", second.is_ok());
    let table = partition::read_table(device.as_mut());
    let Table::Gpt { partitions, .. } = table else {
        report.check("la table relue est une GPT", false);
        return;
    };
    report.check("deux partitions relues", partitions.len() == 2);
    report.check(
        "la premiere intacte (GUID, bornes, nom)",
        partitions.first().is_some_and(|p| p.unique == first.unique && p.first_sector == first.first_sector && p.name == "essai-a"),
    );
    report.check("les deux alignees et de type LIFS", partitions.iter().all(|p| p.aligned() && p.kind == PartitionKind::Lifs));
    let Some(p2) = partitions.get(1).cloned() else {
        return;
    };
    crate::serial_println!(
        "[lifs]   p1 secteurs {}..{}, p2 secteurs {}..{} ({} Mio)",
        partitions[0].first_sector,
        partitions[0].first_sector + partitions[0].sectors - 1,
        p2.first_sector,
        p2.first_sector + p2.sectors - 1,
        p2.sectors / 2048
    );
    let shared: partition::SharedDisk = Arc::new(SpinLock::new(device));
    let Some(window) = PartitionDevice::new(shared.clone(), &p2) else {
        report.check("fenetre sur p2", false);
        return;
    };
    let cache = BlockCache::new(Box::new(window));
    let Some(mut volume) = report.ok("LIFS formate dans p2", Volume::format(cache, "essai-b", 0, now)) else {
        return;
    };
    report.ok("fichier dans p2", Filesystem::write_file(&mut volume, "/f", b"p2", KERNEL, Mode::shared_file(), now));
    report.ok("validation", Filesystem::flush(&mut volume));
    drop(volume);
    // What p2 wrote stayed inside p2: p1's first block is untouched, and
    // the table reads the same.
    let mut raw = [0u8; BLOCK];
    let p1_block = partitions[0].first_sector / 8;
    let untouched = shared.lock().read_block(p1_block, &mut raw).is_ok() && raw.iter().all(|b| *b == 0);
    report.check("p1 non touchee par l'ecriture de p2", untouched);
    let still = matches!(partition::read_table(shared.lock().as_mut()), Table::Gpt { partitions, .. } if partitions.len() == 2);
    report.check("table intacte apres le formatage de p2", still);
    let Some(window) = PartitionDevice::new(shared, &p2) else {
        return;
    };
    match Volume::mount(BlockCache::new(Box::new(window))) {
        Ok(mut again) => {
            report.check("p2 remontee : le fichier y est", Filesystem::read_file(&mut again, "/f", KERNEL).ok().as_deref() == Some(&b"p2"[..]));
            report.ok("p2 coherente", again.check_volume());
        }
        Err(error) => report.check(error.message(), false),
    }
}

/// A directory grown to 300 entries and emptied again: the trees give
/// back the blocks of their splits (`btree::merge_neighbours`).
fn grow_and_shrink(report: &mut Report, now: u64) {
    crate::serial_println!("[lifs] --- 300 fichiers crees puis retires ---");
    let disk = SparseDisk::new(1 << 14);
    let Some(mut volume) = report.ok("volume d'essai", Volume::format(disk.cache(), "croissance", 0, now)) else {
        return;
    };
    let v = &mut volume;
    report.ok("dossier /d", Filesystem::create_path(v, "/d", KERNEL, Mode::shared_dir(), now));
    report.ok("validation", Filesystem::flush(v));
    let before = v.sb.free_blocks;
    let mut created = true;
    for i in 0..300 {
        created &= Filesystem::write_file(v, &alloc::format!("/d/f{}", i), b"x", KERNEL, Mode::shared_file(), now).is_ok();
    }
    report.check("300 fichiers crees", created);
    report.ok("validation", Filesystem::flush(v));
    let grown = v.sb.free_blocks;
    let mut removed = true;
    for i in 0..300 {
        removed &= Filesystem::delete(v, &alloc::format!("/d/f{}", i), KERNEL, now).is_ok();
    }
    report.check("300 fichiers retires", removed);
    report.ok("validation", Filesystem::flush(v));
    crate::serial_println!("[lifs]   blocs libres : {} avant, {} a 300 fichiers, {} apres", before, grown, v.sb.free_blocks);
    report.check("les arbres rendent les blocs de leurs coupures", v.sb.free_blocks == before);
    report.ok("coherent", v.check_volume());
}

/// Runs the LIFS self-test. Returns (passed, failed).
pub fn run(now: u64) -> (usize, usize) {
    let mut report = Report { passed: 0, failed: 0 };
    crate::serial_println!("[lifs] autotest de LIFS (phase 14)");
    clone_and_tags(&mut report, now);
    grow_and_shrink(&mut report, now);
    power_cuts(&mut report, now);
    retarget_cuts(&mut report, now);
    partitions(&mut report, now);
    crate::serial_println!("[lifs] autotest termine : {} reussites, {} echecs", report.passed, report.failed);
    (report.passed, report.failed)
}
